use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

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
    let graph = Graph::build_with_force(workflow, force)?;
    let inputs: BTreeMap<_, &Value> = workflow
        .inputs
        .iter()
        .filter_map(|(name, input)| input.default.as_ref().map(|value| (name, value)))
        .collect();
    let context = json!({ "inputs": inputs });
    let mut missing = BTreeSet::new();

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
            missing.insert(format!("{program:?} (step {})", step.id));
        }
    }

    if missing.is_empty() {
        Ok(graph)
    } else {
        Err(Error::MissingExecutables(missing.into_iter().collect()))
    }
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
