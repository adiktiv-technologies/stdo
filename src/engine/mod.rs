mod budget;
mod cache;
mod ledger;
pub mod runner;
pub(crate) mod telemetry;
pub mod template;

use std::{collections::BTreeMap, io, sync::Arc};

use minijinja::{context, value::Object, Value as TemplateValue};
use serde_json::Value;
use tokio::{sync::watch, task::JoinSet};

use crate::{
    dag::{
        graph::Graph,
        schema::{CommandStep, Input, InputKind, StepKind, Workflow},
    },
    error::Error,
};

pub use self::ledger::ProcessLedger;
pub use self::telemetry::Telemetry;
use self::{
    budget::{Budget, Buffer},
    runner::{Captured, Running},
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
    let telemetry = options.telemetry;
    let result = run_workflow(workflow, overrides, ledger, output, options).await;
    if let Some(telemetry) = telemetry {
        telemetry.finish();
    }
    result
}

async fn run_workflow(
    workflow: &Workflow,
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

    for (layer_number, layer) in (1..).zip(&graph.layers) {
        let _layer_timing = telemetry.map(|report| report.layer(layer_number));
        let context = context! { inputs => &inputs, steps => &outputs };
        let mut commands = Vec::new();
        for &index in layer {
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
                    let input = command_input(command, &context, &outputs, &budget, &step.id)?;
                    commands.push((index, step.id.as_str(), command, argv, input));
                }
            }
        }

        let (cancel, _) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let mut error = None;
        for (index, id, command, argv, input) in commands {
            let mut timing = telemetry.map(|report| report.step(layer_number, index, id));
            let entry = (command.cache && !no_cache).then(|| {
                cache::Entry::new(&argv, command, input.as_deref().map_or(&[], Buffer::bytes))
            });
            let parse_json = command.parse_stdout.is_some();
            if let Some(entry) = &entry {
                let read_started = timing.as_ref().map(|_| std::time::Instant::now());
                match entry.load(&budget, id).await {
                    Ok(Some(captured)) if captured.status.success() || command.ignore_exit_code => {
                        let read_finished =
                            read_started.map(|started| (started, std::time::Instant::now()));
                        // A malformed JSON payload is a cache miss, just like a truncated entry.
                        if let Ok(value) = StepOutput::new(captured, parse_json, id) {
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
                            }
                            outputs.insert(id.to_owned(), TemplateValue::from_object(value));
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
            let running =
                match Running::spawn(id, argv, input, command.ignore_exit_code, ledger.clone()) {
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
            let receiver = cancel.subscribe();
            let id = id.to_owned();
            tasks.spawn(async move {
                let Some(captured) = running.finish(budget, receiver, timing).await? else {
                    return Ok(None);
                };
                let value = StepOutput::new(captured, parse_json, &id)?;
                if let Some(entry) = entry {
                    entry.store(&value.captured).await?;
                }
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
    fn new(captured: Captured, parse_json: bool, step: &str) -> Result<Self, Error> {
        let json = if parse_json {
            let value: Value =
                serde_json::from_slice(captured.stdout.bytes()).map_err(|source| Error::Json {
                    step: step.to_owned(),
                    source,
                })?;
            Some(TemplateValue::from_serialize(value))
        } else {
            None
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
