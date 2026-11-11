//! Static discovery uses the same working-directory path semantics as child processes.
use crate::{
    dag::{
        graph::Graph,
        schema::{StepKind, Workflow},
    },
    engine::template,
    error::Error,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub struct Invocation {
    pub path: PathBuf,
    pub workflow: Workflow,
    pub inputs: Vec<String>,
}

pub fn context(workflow: &Workflow, overrides: &[String]) -> Value {
    let mut inputs: BTreeMap<String, Value> = workflow
        .inputs
        .iter()
        .filter_map(|(name, input)| input.default.clone().map(|value| (name.clone(), value)))
        .collect();
    for argument in overrides {
        if let Some((key, value)) = argument.split_once('=') {
            let typed = workflow.inputs.get(key).is_some_and(|input| {
                matches!(
                    input.kind,
                    crate::dag::schema::InputKind::Boolean | crate::dag::schema::InputKind::Number
                )
            });
            inputs.insert(
                key.to_owned(),
                if typed {
                    serde_json::from_str(value).unwrap_or_else(|_| json!(value))
                } else {
                    json!(value)
                },
            );
        }
    }
    json!({"inputs":inputs})
}

/// Find the named workflow file in a direct `stdo run` invocation.
pub fn reference(argv: &[String]) -> Option<&str> {
    if Path::new(argv.first()?).file_name()? != "stdo" || argv.get(1)? != "run" {
        return None;
    }
    file_argument(argv[2..].iter().map(|arg| Some(arg.as_str()))).flatten()
}

pub(crate) const CALL_STACK_ENV: &str = "STDO_CALL_STACK";
const MAX_SUBWORKFLOW_DEPTH: usize = 16;
const MAX_SUBWORKFLOW_CALLS: usize = 64;
const DIGEST_LEN: usize = 64;

fn workflow_id(path: &Path) -> String {
    crate::report::sha256(path.as_os_str().as_encoded_bytes())
}

fn inherited_stack() -> Result<Vec<String>, Error> {
    let Some(value) = std::env::var_os(CALL_STACK_ENV) else {
        return Ok(Vec::new());
    };
    let invalid = || Error::InvalidWorkflow(format!("invalid {CALL_STACK_ENV}"));
    let value = value.to_str().ok_or_else(invalid)?;
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.len() > MAX_SUBWORKFLOW_DEPTH * (DIGEST_LEN + 1) {
        return Err(depth_error());
    }
    value
        .split(':')
        .map(|id| {
            if id.len() == DIGEST_LEN && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                Ok(id.to_ascii_lowercase())
            } else {
                Err(invalid())
            }
        })
        .collect()
}

fn depth_error() -> Error {
    Error::InvalidWorkflow(format!(
        "maximum sub-workflow nesting depth ({MAX_SUBWORKFLOW_DEPTH}) exceeded"
    ))
}

/// Validate the discoverable call tree and prepare the environment inherited by every step.
pub(crate) fn execution_stack(
    workflow: &Workflow,
    source: Option<&Path>,
    overrides: &[String],
    force: bool,
) -> Result<String, Error> {
    let mut ancestors = inherited_stack()?;
    let source = source
        .map(|path| {
            path.canonicalize().map_err(|source| Error::ReadWorkflow {
                path: path.to_owned(),
                source,
            })
        })
        .transpose()?;
    let id = source.as_deref().map(workflow_id);
    if id.as_ref().is_some_and(|id| ancestors.contains(id)) {
        return Err(Error::InvalidWorkflow(format!(
            "sub-workflow cycle detected: {}",
            source.as_deref().expect("identified path").display()
        )));
    }
    if ancestors.len() >= MAX_SUBWORKFLOW_DEPTH {
        return Err(depth_error());
    }
    let root = std::env::current_dir()
        .map_err(|error| Error::InvalidWorkflow(format!("workspace: {error}")))?;
    discover(
        workflow,
        &root,
        source.as_deref(),
        overrides,
        force,
        Some(&ancestors),
    )?;
    ancestors.extend(id);
    Ok(ancestors.join(":"))
}

pub fn collect(
    workflow: &Workflow,
    root: &Path,
    source: Option<&Path>,
    overrides: &[String],
    force: bool,
) -> Result<Vec<Invocation>, Error> {
    discover(workflow, root, source, overrides, force, None)
}

fn discover(
    workflow: &Workflow,
    root: &Path,
    source: Option<&Path>,
    overrides: &[String],
    force: bool,
    ancestors: Option<&[String]>,
) -> Result<Vec<Invocation>, Error> {
    let root = root
        .canonicalize()
        .map_err(|error| Error::InvalidWorkflow(format!("workspace: {error}")))?;
    let stack = source
        .and_then(|path| path.canonicalize().ok())
        .into_iter()
        .collect();
    let mut discovery = Discovery {
        root,
        stack,
        found: Vec::new(),
        force,
        ancestors,
    };
    discovery.visit(workflow, Some(overrides))?;
    Ok(discovery.found)
}

struct Discovery<'a> {
    root: PathBuf,
    stack: Vec<PathBuf>,
    found: Vec<Invocation>,
    force: bool,
    // Execution permits values that can only be resolved after earlier steps run.
    ancestors: Option<&'a [String]>,
}

impl Discovery<'_> {
    fn visit(&mut self, workflow: &Workflow, overrides: Option<&[String]>) -> Result<(), Error> {
        let context =
            overrides.map_or_else(|| json!({"inputs": {}}), |args| context(workflow, args));
        for step in &workflow.steps {
            let StepKind::Command(command) = &step.kind else {
                continue;
            };
            let Some(executable) = command.cmd.first() else {
                continue;
            };
            let render = |arg: &str| match template::render(arg, &context) {
                Ok(value) => Ok(Some(value)),
                Err(error)
                    if self.ancestors.is_some()
                        && error.kind() == minijinja::ErrorKind::UndefinedError =>
                {
                    Ok(None)
                }
                Err(source) => Err(Error::Template {
                    location: format!("sub-workflow step {:?}", step.id),
                    source,
                }),
            };
            let Some(program) = render(executable)? else {
                continue;
            };
            if Path::new(&program)
                .file_name()
                .is_none_or(|name| name != "stdo")
            {
                continue;
            }
            let Some(subcommand) = command.cmd.get(1) else {
                continue;
            };
            if render(subcommand)?.as_deref() != Some("run") {
                continue;
            }
            let args = command.cmd[2..]
                .iter()
                .map(|arg| render(arg))
                .collect::<Result<Vec<_>, _>>()?;
            let Some(Some(raw)) = file_argument(args.iter().map(Option::as_deref)) else {
                continue;
            };
            if raw.is_empty() {
                return Err(Error::InvalidWorkflow(format!(
                    "sub-workflow step {} needs a YAML file after -f or --file",
                    step.id
                )));
            }
            let path = self.child_path(raw)?;
            let child = Workflow::read(&path)?;
            Graph::build_with_force(&child, self.force)?;
            let inputs = input_arguments(&args);
            // Unresolved arguments can override child inputs. Discover literal child calls
            // without assuming defaults that may differ at runtime.
            let child_overrides = args
                .iter()
                .all(Option::is_some)
                .then_some(inputs.as_slice());
            self.stack.push(path.clone());
            self.visit(&child, child_overrides)?;
            self.stack.pop();
            self.found.push(Invocation {
                path,
                workflow: child,
                inputs,
            });
        }
        Ok(())
    }

    fn child_path(&self, raw: &str) -> Result<PathBuf, Error> {
        let path =
            self.root.join(raw).canonicalize().map_err(|error| {
                Error::InvalidWorkflow(format!("sub-workflow {raw:?}: {error}"))
            })?;
        if !path.starts_with(&self.root)
            || !matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("yaml" | "yml")
            )
        {
            return Err(Error::InvalidWorkflow(format!(
                "sub-workflow {raw:?} must be a YAML file inside the workspace"
            )));
        }
        if self.stack.contains(&path) {
            let chain = self
                .stack
                .iter()
                .chain(std::iter::once(&path))
                .map(|path| {
                    path.strip_prefix(&self.root)
                        .unwrap_or(path)
                        .display()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(Error::InvalidWorkflow(format!(
                "sub-workflow cycle detected: {chain}"
            )));
        }
        let ancestors = self.ancestors.unwrap_or_default();
        if ancestors.contains(&workflow_id(&path)) {
            return Err(Error::InvalidWorkflow(format!(
                "sub-workflow cycle detected: {raw} is already active"
            )));
        }
        if ancestors.len() + self.stack.len() >= MAX_SUBWORKFLOW_DEPTH {
            return Err(depth_error());
        }
        if self.found.len() + self.stack.len() >= MAX_SUBWORKFLOW_CALLS {
            return Err(Error::InvalidWorkflow(format!(
                "sub-workflow validation limit exceeded ({MAX_SUBWORKFLOW_CALLS} calls)"
            )));
        }
        let metadata = path
            .metadata()
            .map_err(|error| Error::InvalidWorkflow(error.to_string()))?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            return Err(Error::InvalidWorkflow(format!(
                "sub-workflow {raw:?} must be a regular YAML file up to 1 MiB"
            )));
        }
        Ok(path)
    }
}

fn file_argument<'a>(mut args: impl Iterator<Item = Option<&'a str>>) -> Option<Option<&'a str>> {
    while let Some(arg) = args.next() {
        let Some(arg) = arg else { continue };
        if matches!(arg, "-f" | "--file") {
            return Some(args.next().unwrap_or(Some("")));
        }
        if let Some(path) = arg.strip_prefix("--file=") {
            return Some(Some(path));
        }
    }
    None
}

fn input_arguments(args: &[Option<String>]) -> Vec<String> {
    let mut inputs = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_deref() {
            Some("-i" | "--input") => {
                if let Some(Some(value)) = args.next() {
                    inputs.push(value.clone());
                }
            }
            Some(arg) => {
                if let Some(value) = arg.strip_prefix("--input=") {
                    inputs.push(value.to_owned());
                }
            }
            None => {}
        }
    }
    inputs
}
