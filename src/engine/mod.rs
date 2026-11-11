mod budget;
mod cache;
pub(crate) mod events;
mod jsonl;
mod ledger;
mod pipe;
pub mod runner;
pub(crate) mod telemetry;
pub mod template;

use std::{collections::BTreeMap, io, path::Path, sync::Arc, time::Duration};

use minijinja::{context, value::Object, Value as TemplateValue};
use serde_json::Value;
use tokio::{sync::watch, task::JoinSet};

use crate::{
    dag::{
        graph::Graph,
        schema::{CommandStep, Input, InputKind, PipeMode, StdoutFormat, StepKind, Workflow},
    },
    doctor::subworkflows,
    error::Error,
};

pub use self::ledger::ProcessLedger;
pub use self::telemetry::Telemetry;
use self::{
    budget::{Budget, Buffer},
    runner::{Captured, Running, SpawnContext},
    telemetry::Status,
};

type Inputs = BTreeMap<String, Value>;
type Outputs = BTreeMap<String, TemplateValue>;

pub async fn run(
    workflow: &Workflow,
    overrides: &[String],
    force: bool,
    ledger: &ProcessLedger,
    output: &mut impl io::Write,
) -> Result<(), Error> {
    run_with_telemetry(workflow, overrides, force, ledger, output, None).await
}

pub async fn run_with_telemetry(
    workflow: &Workflow,
    overrides: &[String],
    force: bool,
    ledger: &ProcessLedger,
    output: &mut impl io::Write,
    telemetry: Option<&Telemetry>,
) -> Result<(), Error> {
    run_with_options(
        workflow,
        overrides,
        ledger,
        output,
        RunOptions {
            force,
            telemetry,
            ..RunOptions::default()
        },
    )
    .await
}

#[derive(Default)]
pub struct RunOptions<'a> {
    pub force: bool,
    pub no_cache: bool,
    pub telemetry: Option<&'a Telemetry>,
}

pub async fn run_with_options(
    workflow: &Workflow,
    overrides: &[String],
    ledger: &ProcessLedger,
    output: &mut impl io::Write,
    options: RunOptions<'_>,
) -> Result<(), Error> {
    run_with_source(workflow, None, overrides, ledger, output, options).await
}

/// Execute a workflow with its file identity available for recursive call checks.
pub async fn run_with_source(
    workflow: &Workflow,
    source: Option<&Path>,
    overrides: &[String],
    ledger: &ProcessLedger,
    output: &mut impl io::Write,
    options: RunOptions<'_>,
) -> Result<(), Error> {
    let telemetry = options.telemetry;
    let result = run_workflow(workflow, source, overrides, ledger, output, options).await;
    if let Some(telemetry) = telemetry {
        telemetry.finish();
    }
    result
}

async fn run_workflow(
    workflow: &Workflow,
    source: Option<&Path>,
    overrides: &[String],
    ledger: &ProcessLedger,
    output: &mut impl io::Write,
    options: RunOptions<'_>,
) -> Result<(), Error> {
    let RunOptions {
        force,
        no_cache,
        telemetry,
    } = options;
    let graph = Graph::build_with_force(workflow, force)?;
    let inputs = resolve_inputs(workflow, overrides)?;
    let spawn_context = SpawnContext {
        call_stack: Arc::from(subworkflows::execution_stack(
            workflow, source, overrides, force,
        )?),
        ledger: ledger.clone(),
    };
    let argv_context = context! { inputs => &inputs };
    let budget = telemetry
        .map(|report| report.budget.clone())
        .unwrap_or_default();
    let mut outputs = Outputs::new();
    let mut skipped = vec![false; workflow.steps.len()];
    let indices: BTreeMap<_, _> = workflow
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.id.as_str(), index))
        .collect();

    let plan = graph.execution_plan(workflow, no_cache);
    for group in plan.groups {
        let mut layer_timings = BTreeMap::new();
        let mut pipes = BTreeMap::<usize, pipe::Outputs>::new();
        let context = context! { inputs => &inputs, steps => &outputs };
        let mut commands = Vec::new();
        for (layer_number, index) in group {
            if let Some(report) = telemetry {
                layer_timings
                    .entry(layer_number)
                    .or_insert_with(|| report.layer(layer_number));
            }
            let step = &workflow.steps[index];
            if graph.dependencies[index]
                .iter()
                .any(|&source| skipped[source])
            {
                skipped[index] = true;
            }
            if skipped[index] {
                if let Some(report) = telemetry {
                    report
                        .step(layer_number, index, &step.id)
                        .set_status(Status::Skipped);
                }
                continue;
            }
            match &step.kind {
                StepKind::Switch(switch) => {
                    let mut timing =
                        telemetry.map(|report| report.step(layer_number, index, &step.id));
                    if let Some(timer) = &timing {
                        timer.started();
                    }
                    let result =
                        template::render_bounded(&switch.condition, &context, &budget, &step.id)?;
                    let selected = std::str::from_utf8(result.bytes())
                        .expect("rendered templates are UTF-8")
                        .trim();
                    // MiniJinja renders booleans as True/False; YAML cases commonly use true/false.
                    let target = switch
                        .cases
                        .get(selected)
                        .or_else(|| match selected {
                            "True" => switch.cases.get("true"),
                            "False" => switch.cases.get("false"),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            Error::InvalidWorkflow(format!(
                                "switch {:?} has no case for {selected:?}",
                                step.id
                            ))
                        })?;
                    for candidate in switch
                        .cases
                        .values()
                        .filter(|candidate| *candidate != target)
                    {
                        skipped[indices[candidate.as_str()]] = true;
                    }
                    if let Some(timer) = &mut timing {
                        timer.set_status(Status::Switch(target.clone()));
                    }
                }
                StepKind::Command(command) => {
                    let argv = command
                        .cmd
                        .iter()
                        .map(|argument| {
                            template::render(argument, &argv_context).map_err(|source| {
                                Error::Template {
                                    location: format!(
                                        "command for step {:?} (only inputs are available in argv)",
                                        step.id
                                    ),
                                    source,
                                }
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let input = match plan.live_sources[index] {
                        Some(source) => Some(pipes.entry(source).or_default().connect()),
                        None => command_input(command, &context, &outputs, &budget, &step.id)?
                            .map(pipe::Input::Buffered),
                    };
                    let entry = if command.cache && !no_cache {
                        let paths = command
                            .cache_files
                            .iter()
                            .map(|path| {
                                template::render(path, &argv_context).map_err(|source| {
                                    Error::Template {
                                        location: format!("cache_files for step {:?}", step.id),
                                        source,
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        Some(
                            cache::Entry::new(
                                &step.id,
                                &argv,
                                command,
                                input.as_ref().map_or(&[], pipe::Input::cache_bytes),
                                &paths,
                            )
                            .await?,
                        )
                    } else {
                        None
                    };
                    commands.push((
                        layer_number,
                        index,
                        step.id.as_str(),
                        command,
                        argv,
                        input,
                        entry,
                    ));
                }
            }
        }

        let (cancel, _) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let mut error = None;
        for (layer_number, index, id, command, argv, input, entry) in commands {
            let mut outgoing = pipes.remove(&index).unwrap_or_default();
            let mut timing = telemetry.map(|report| report.step(layer_number, index, id));
            if let Some(timer) = &timing {
                timer.started();
            }
            let format = command.parse_stdout.clone();
            if let Some(entry) = &entry {
                let read_started = timing.as_ref().map(|_| std::time::Instant::now());
                match entry.load(&budget, id).await {
                    Ok(Some(captured)) if captured.status.success() || command.ignore_exit_code => {
                        let read_finished =
                            read_started.map(|started| (started, std::time::Instant::now()));
                        // A malformed JSON payload is a cache miss, just like a truncated entry.
                        let value = match StepOutput::new(captured, format.as_ref(), id, true) {
                            Ok(value) => Some(value),
                            Err(source @ Error::MemoryLimitExceeded { .. }) => {
                                error = Some(source);
                                cancel.send_replace(true);
                                break;
                            }
                            Err(_) => None,
                        };
                        if let Some(value) = value {
                            if let Some(timer) = &mut timing {
                                if let Some((started, finished)) = read_finished {
                                    timer.cache_read(
                                        started,
                                        finished,
                                        !value.captured.stdout.bytes().is_empty(),
                                    );
                                }
                                timer.set_status(Status::CacheHit(value.captured.status));
                                timer.set_streams(
                                    value.captured.stdout.bytes(),
                                    value.captured.stderr.bytes(),
                                );
                                timer.stdout_chunk(value.captured.stdout.bytes());
                            }
                            let budget = budget.clone();
                            let mut receiver = cancel.subscribe();
                            let id = id.to_owned();
                            tasks.spawn(async move {
                                let _timing = timing;
                                tokio::select! {
                                    biased;
                                    _ = receiver.wait_for(|cancelled| *cancelled) => Ok(None),
                                    result = async {
                                        outgoing.forward(value.captured.stdout.bytes(), &budget, &id).await?;
                                        outgoing.finish().await;
                                        Ok(Some((id, TemplateValue::from_object(value))))
                                    } => result,
                                }
                            });
                            continue;
                        }
                    }
                    Ok(_) => {}
                    Err(source) => {
                        error = Some(source);
                        cancel.send_replace(true);
                        break;
                    }
                }
            }
            let retry = command.retry;
            let retries = command.retries();
            let capture_stdout = command.capture_stdout;
            let replay = if retries {
                input.as_ref().map(pipe::Input::replay)
            } else {
                None
            };
            let spawn_context = spawn_context.clone();
            let ignore_exit_code = command.ignore_exit_code;
            let timeout_ms = command.timeout_ms;
            let spawn = move |id: &str, input| {
                Running::spawn(
                    id,
                    &argv,
                    input,
                    ignore_exit_code,
                    timeout_ms,
                    !retries,
                    &spawn_context,
                )
            };
            let mut running = match spawn(id, input) {
                Ok(running) => running,
                Err(source) => {
                    error = Some(source);
                    cancel.send_replace(true);
                    break;
                }
            };
            if let Some(timer) = &mut timing {
                timer.spawned();
            }
            let budget = budget.clone();
            let mut receiver = cancel.subscribe();
            let id = id.to_owned();
            tasks.spawn(async move {
                let mut attempt = 1;
                let value = loop {
                    let group = running.group();
                    let result = match running
                        .finish(
                            budget.clone(),
                            receiver.clone(),
                            timing.as_mut(),
                            format == Some(StdoutFormat::Jsonl),
                            capture_stdout,
                            &mut outgoing,
                        )
                        .await
                    {
                        Ok(None) => return Ok(None),
                        Ok(Some(captured)) => {
                            let buffered_format = format.as_ref().filter(|_| capture_stdout);
                            let value = StepOutput::new(captured, buffered_format, &id, false);
                            if value.is_err() {
                                runner::kill_group(group).await;
                            }
                            value
                        }
                        Err(error) => Err(error),
                    };
                    let failure = match result {
                        Ok(value) => break value,
                        Err(error) => error,
                    };
                    if matches!(failure, Error::Json { .. } | Error::JsonLine { .. }) {
                        if let Some(timer) = &mut timing {
                            timer.set_status(Status::Failed);
                        }
                    }
                    let Some(policy) = retry.filter(|policy| {
                        attempt < policy.attempts
                            && matches!(
                                failure,
                                Error::StepFailed { .. }
                                    | Error::StepTimeout { .. }
                                    | Error::Json { .. }
                                    | Error::JsonLine { .. }
                            )
                    }) else {
                        return Err(failure);
                    };
                    tokio::select! {
                        biased;
                        _ = receiver.wait_for(|cancelled| *cancelled) => {
                            if let Some(timer) = &mut timing {
                                timer.set_status(Status::Cancelled);
                            }
                            return Ok(None);
                        }
                        _ = tokio::time::sleep(Duration::from_millis(policy.backoff_ms)) => {}
                    }
                    running =
                        spawn(&id, replay.as_ref().map(pipe::Input::replay)).inspect_err(|_| {
                            if let Some(timer) = &mut timing {
                                timer.set_status(Status::Failed);
                            }
                        })?;
                    attempt += 1;
                    if let Some(timer) = &mut timing {
                        timer.spawned();
                    }
                };
                if let Some(entry) = entry {
                    entry.store(&value.captured).await.inspect_err(|_| {
                        if let Some(timer) = &mut timing {
                            timer.set_status(Status::Failed);
                        }
                    })?;
                }
                if retries {
                    if let Some(timer) = &timing {
                        timer.stdout_chunk(value.captured.stdout.bytes());
                    }
                }
                outgoing.finish().await;
                Ok(Some((id, TemplateValue::from_object(value))))
            });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(Some((id, value)))) => {
                    outputs.insert(id, value);
                }
                Ok(Ok(None)) => {}
                failure => {
                    if error.is_none() {
                        error = Some(match failure {
                            Ok(Err(source)) => source,
                            Err(source) => {
                                Error::InvalidWorkflow(format!("step task failed: {source}"))
                            }
                            Ok(Ok(_)) => unreachable!(),
                        });
                        cancel.send_replace(true);
                    }
                }
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
    }

    if let Some(source) = &workflow.output {
        let context = context! { inputs => &inputs, steps => &outputs };
        let rendered = template::render_bounded(source, &context, &budget, "workflow output")?;
        output
            .write_all(rendered.bytes())
            .map_err(Error::WriteOutput)?;
    }
    Ok(())
}

fn command_input(
    command: &CommandStep,
    context: &TemplateValue,
    outputs: &Outputs,
    budget: &Budget,
    step: &str,
) -> Result<Option<Arc<Buffer>>, Error> {
    if let Some(source) = &command.pipe_from {
        let upstream = outputs
            .get(source)
            .and_then(TemplateValue::downcast_object_ref::<StepOutput>)
            .ok_or_else(|| {
                Error::InvalidWorkflow(format!("step {step:?} has no pipe output from {source:?}"))
            })?;
        if command.pipe_mode == PipeMode::Atomic && !upstream.captured.status.success() {
            return Err(Error::AtomicPipe {
                step: step.to_owned(),
                producer: source.clone(),
                status: upstream.captured.status,
            });
        }
        return Ok(Some(upstream.captured.stdout.clone()));
    }
    command
        .stdin
        .as_ref()
        .map(|source| template::render_bounded(source, context, budget, step).map(Arc::new))
        .transpose()
}

struct StepOutput {
    captured: Captured,
    json: Option<TemplateValue>,
}

impl StepOutput {
    fn new(
        captured: Captured,
        format: Option<&StdoutFormat>,
        step: &str,
        cached: bool,
    ) -> Result<Self, Error> {
        let json = match format {
            Some(StdoutFormat::Json) => {
                let value: Value =
                    serde_json::from_slice(captured.stdout.bytes()).map_err(|source| {
                        Error::Json {
                            step: step.to_owned(),
                            source,
                        }
                    })?;
                Some(TemplateValue::from_serialize(value))
            }
            Some(StdoutFormat::Jsonl) => Some(TemplateValue::from_object(jsonl::Records::new(
                captured.stdout.clone(),
                step,
                cached,
            )?)),
            None => None,
        };
        Ok(Self { captured, json })
    }
}

impl std::fmt::Debug for StepOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("StepOutput").finish_non_exhaustive()
    }
}

impl Object for StepOutput {
    fn get_value_by_str(self: &Arc<Self>, key: &str) -> Option<TemplateValue> {
        match key {
            "stdout" => Some(
                String::from_utf8_lossy(self.captured.stdout.bytes())
                    .into_owned()
                    .into(),
            ),
            "stderr" => Some(
                String::from_utf8_lossy(self.captured.stderr.bytes())
                    .into_owned()
                    .into(),
            ),
            "exit_code" => self.captured.status.code().map(TemplateValue::from),
            "json" => self.json.clone(),
            _ => None,
        }
    }
}

pub(crate) fn resolve_inputs(workflow: &Workflow, overrides: &[String]) -> Result<Inputs, Error> {
    let mut inputs = Inputs::new();
    for (name, input) in &workflow.inputs {
        if let Some(default) = &input.default {
            if !valid_input(input, default) {
                return Err(Error::InvalidWorkflow(format!(
                    "invalid default for input {name:?}"
                )));
            }
            inputs.insert(name.clone(), default.clone());
        }
    }
    for argument in overrides {
        let (name, raw) = argument.split_once('=').ok_or_else(|| {
            Error::InvalidWorkflow(format!("expected KEY=VALUE for input {argument:?}"))
        })?;
        let input = workflow
            .inputs
            .get(name)
            .ok_or_else(|| Error::InvalidWorkflow(format!("unknown input {name:?}")))?;
        let value = match input.kind {
            InputKind::Number | InputKind::Boolean => serde_json::from_str(raw).ok(),
            _ => Some(Value::String(raw.to_owned())),
        }
        .filter(|value| valid_input(input, value))
        .ok_or_else(|| Error::InvalidWorkflow(format!("invalid value for input {name:?}")))?;
        inputs.insert(name.to_owned(), value);
    }
    Ok(inputs)
}

fn valid_input(input: &Input, value: &Value) -> bool {
    match input.kind {
        InputKind::String | InputKind::Path => value.is_string(),
        InputKind::Enum => value
            .as_str()
            .is_some_and(|value| input.values.iter().any(|choice| choice == value)),
        InputKind::Number => value.is_number(),
        InputKind::Boolean => value.is_boolean(),
    }
}
