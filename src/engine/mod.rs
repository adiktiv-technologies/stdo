mod budget;
mod ledger;
pub mod runner;
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
use self::{
    budget::{Budget, Buffer},
    runner::{Captured, Running},
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
    let graph = Graph::build_with_force(workflow, force)?;
    let inputs = resolve_inputs(workflow, overrides)?;
    let argv_context = context! { inputs => &inputs };
    let budget = Budget::default();
    let mut outputs = Outputs::new();
    let mut skipped = vec![false; workflow.steps.len()];
    let indices: BTreeMap<_, _> = workflow
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.id.as_str(), index))
        .collect();

    for layer in &graph.layers {
        let context = context! { inputs => &inputs, steps => &outputs };
        let mut commands = Vec::new();
        for &index in layer {
            if graph.dependencies[index]
                .iter()
                .any(|&source| skipped[source])
            {
                skipped[index] = true;
            }
            if skipped[index] {
                continue;
            }
            let step = &workflow.steps[index];
            match &step.kind {
                StepKind::Switch(switch) => {
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
                    commands.push((step.id.as_str(), command, argv, input));
                }
            }
        }

        let (cancel, _) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let mut error = None;
        for (id, command, argv, input) in commands {
            let running =
                match Running::spawn(id, argv, input, command.ignore_exit_code, ledger.clone()) {
                    Ok(running) => running,
                    Err(source) => {
                        error = Some(source);
                        cancel.send_replace(true);
                        break;
                    }
                };
            let budget = budget.clone();
            let receiver = cancel.subscribe();
            let parse_json = command.parse_stdout.is_some();
            let id = id.to_owned();
            tasks.spawn(async move {
                let Some(captured) = running.finish(budget, receiver).await? else {
                    return Ok(None);
                };
                let json = if parse_json {
                    let json: Value =
                        serde_json::from_slice(captured.stdout.bytes()).map_err(|source| {
                            Error::Json {
                                step: id.clone(),
                                source,
                            }
                        })?;
                    Some(TemplateValue::from_serialize(json))
                } else {
                    None
                };
                Ok(Some((
                    id,
                    TemplateValue::from_object(StepOutput { captured, json }),
                )))
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

fn resolve_inputs(workflow: &Workflow, overrides: &[String]) -> Result<Inputs, Error> {
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
