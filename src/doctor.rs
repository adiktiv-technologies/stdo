use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    os::unix::fs::PermissionsExt,
    path::Path,
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
    let graph = Graph::build(workflow)?;
    let inputs: BTreeMap<_, &Value> = workflow
        .inputs
        .iter()
        .filter_map(|(name, input)| input.default.as_ref().map(|value| (name, value)))
        .collect();
    let context = json!({ "inputs": inputs });
    let search_path = env::var_os("PATH");
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
        let available = if program.contains('/') {
            is_executable(Path::new(&program))
        } else {
            search_path.as_ref().is_some_and(|path| {
                env::split_paths(path).any(|directory| is_executable(&directory.join(&program)))
            })
        };
        if !available {
            missing.insert(format!("{program:?} (step {})", step.id));
        }
    }

    if missing.is_empty() {
        Ok(graph)
    } else {
        Err(Error::MissingExecutables(missing.into_iter().collect()))
    }
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}
