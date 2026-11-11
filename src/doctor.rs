pub mod subworkflows;

use std::{
    collections::BTreeSet,
    env,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use crate::{
    dag::{
        graph::Graph,
        schema::{StepKind, Workflow},
    },
    engine::template,
    error::Error,
};

/// Validates the workflow and checks each declared command without spawning it.
pub fn check(workflow: &Workflow) -> Result<Graph, Error> {
    check_with_force(workflow, false)
}

pub fn check_with_force(workflow: &Workflow, force: bool) -> Result<Graph, Error> {
    let root = env::current_dir().map_err(Error::Studio)?;
    check_in_workspace(workflow, &root, None, &[], force)
}

pub fn check_in_workspace(
    workflow: &Workflow,
    root: &Path,
    source: Option<&Path>,
    overrides: &[String],
    force: bool,
) -> Result<Graph, Error> {
    inspect_in_workspace(workflow, root, source, overrides, force).map(|(graph, _)| graph)
}

pub fn inspect_in_workspace(
    workflow: &Workflow,
    root: &Path,
    source: Option<&Path>,
    overrides: &[String],
    force: bool,
) -> Result<(Graph, BTreeSet<String>), Error> {
    let graph = Graph::build_with_force(workflow, force)?;
    let children = subworkflows::collect(workflow, root, source, overrides, force)?;
    let mut missing = BTreeSet::new();
    let mut models = BTreeSet::new();
    check_commands(workflow, overrides, None, &mut missing, &mut models)?;
    for child in children {
        check_commands(
            &child.workflow,
            &child.inputs,
            Some(&child.path),
            &mut missing,
            &mut models,
        )?;
    }
    if missing.is_empty() {
        Ok((graph, models))
    } else {
        Err(Error::MissingExecutables(missing.into_iter().collect()))
    }
}

fn check_commands(
    workflow: &Workflow,
    overrides: &[String],
    source: Option<&Path>,
    missing: &mut BTreeSet<String>,
    models: &mut BTreeSet<String>,
) -> Result<(), Error> {
    let context = subworkflows::context(workflow, overrides);
    for name in ["llm_model", "tts_model"] {
        if let Some(model) = context["inputs"][name]
            .as_str()
            .filter(|value| !value.is_empty())
        {
            models.insert(model.to_owned());
        }
    }
    for step in &workflow.steps {
        let StepKind::Command(command) = &step.kind else {
            continue;
        };
        let program =
            template::render(&command.cmd[0], &context).map_err(|source| Error::Template {
                location: format!("executable for step {:?}", step.id),
                source,
            })?;
        if resolve_executable(&program).is_none() {
            missing.insert(format!(
                "{program:?} (step {}{})",
                step.id,
                source
                    .map(|path| format!(", sub-workflow ({})", path.display()))
                    .unwrap_or_default()
            ));
        }
        if Path::new(&program)
            .file_name()
            .is_some_and(|name| name == "ollama")
            && command.cmd.get(1).is_some_and(|arg| arg == "run")
        {
            if let Some(model) = command
                .cmd
                .get(2)
                .and_then(|arg| template::render(arg, &context).ok())
            {
                models.insert(model);
            }
        }
    }

    Ok(())
}

pub(crate) fn resolve_executable(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if program.contains('/') {
        return is_executable(path).then(|| path.to_owned());
    }
    let search_path = env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    env::split_paths(&search_path)
        .map(|directory| directory.join(program))
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}
