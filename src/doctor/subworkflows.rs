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

/// Finds a direct `stdo run` call; shell scripts and runtime-generated commands are not expanded.
pub fn reference(argv: &[String]) -> Option<&str> {
    if Path::new(argv.first()?).file_name()?.to_str()? != "stdo" || argv.get(1)? != "run" {
        return None;
    }
    argv.iter().enumerate().skip(2).find_map(|(index, arg)| {
        if matches!(arg.as_str(), "-f" | "--file") {
            Some(argv.get(index + 1).map_or("", String::as_str))
        } else {
            arg.strip_prefix("--file=")
        }
    })
}

pub fn collect(
    workflow: &Workflow,
    root: &Path,
    source: Option<&Path>,
    overrides: &[String],
    force: bool,
) -> Result<Vec<Invocation>, Error> {
    let root = root
        .canonicalize()
        .map_err(|err| Error::InvalidWorkflow(format!("workspace: {err}")))?;
    let mut stack = Vec::new();
    if let Some(source) = source {
        if let Ok(path) = source.canonicalize() {
            stack.push(path);
        }
    }
    let mut found = Vec::new();
    visit(workflow, &root, overrides, force, &mut stack, &mut found)?;
    Ok(found)
}

fn visit(
    workflow: &Workflow,
    root: &Path,
    overrides: &[String],
    force: bool,
    stack: &mut Vec<PathBuf>,
    found: &mut Vec<Invocation>,
) -> Result<(), Error> {
    let context = context(workflow, overrides);
    for step in &workflow.steps {
        let StepKind::Command(command) = &step.kind else {
            continue;
        };
        // Render only the executable before deciding whether the other arguments matter.
        let Some(executable) = command.cmd.first() else {
            continue;
        };
        let program = template::render(executable, &context)
            .map_err(|err| Error::InvalidWorkflow(format!("step {} executable: {err}", step.id)))?;
        let mut args = command.cmd.clone();
        args[0] = program;
        if reference(&args).is_none() {
            continue;
        }
        for arg in args.iter_mut().skip(1) {
            *arg = template::render(arg, &context).map_err(|err| {
                Error::InvalidWorkflow(format!(
                    "sub-workflow step {} requires statically resolvable arguments: {err}",
                    step.id
                ))
            })?;
        }
        let raw = reference(&args)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| {
                Error::InvalidWorkflow(format!(
                    "sub-workflow step {} needs a YAML file after -f or --file",
                    step.id
                ))
            })?;
        let path = root
            .join(raw)
            .canonicalize()
            .map_err(|err| Error::InvalidWorkflow(format!("sub-workflow {raw:?}: {err}")))?;
        if !path.starts_with(root)
            || !matches!(
                path.extension().and_then(|v| v.to_str()),
                Some("yaml" | "yml")
            )
        {
            return Err(Error::InvalidWorkflow(format!(
                "sub-workflow {raw:?} must be a YAML file inside the workspace"
            )));
        }
        if stack.contains(&path) {
            let mut chain: Vec<_> = stack
                .iter()
                .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
                .collect();
            chain.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
            return Err(Error::InvalidWorkflow(format!(
                "sub-workflow cycle: {}",
                chain.join(" -> ")
            )));
        }
        if stack.len() >= 16 || found.len() + stack.len() >= 64 {
            return Err(Error::InvalidWorkflow(
                "sub-workflow validation limit exceeded (depth 16, 64 calls)".into(),
            ));
        }
        let metadata = path
            .metadata()
            .map_err(|err| Error::InvalidWorkflow(err.to_string()))?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            return Err(Error::InvalidWorkflow(format!(
                "sub-workflow {raw:?} must be a regular YAML file up to 1 MiB"
            )));
        }
        let child = Workflow::read(&path)?;
        Graph::build_with_force(&child, force)?;
        let mut inputs = Vec::new();
        let mut index = 2;
        while index < args.len() {
            match args[index].as_str() {
                "-i" | "--input" => {
                    index += 1;
                    if let Some(value) = args.get(index) {
                        inputs.push(value.clone());
                    }
                }
                arg if arg.starts_with("--input=") => inputs.push(arg[8..].to_owned()),
                _ => {}
            }
            index += 1;
        }
        stack.push(path.clone());
        visit(&child, root, &inputs, force, stack, found)?;
        stack.pop();
        found.push(Invocation {
            path,
            workflow: child,
            inputs,
        });
    }
    Ok(())
}
