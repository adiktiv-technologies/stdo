use std::collections::{BTreeSet, HashMap};

use minijinja::Environment;

use crate::{engine::template, error::Error};

use super::schema::{PipeMode, StepKind, Workflow};

pub const MAX_STEPS: usize = 64;
pub const MAX_DEPTH: usize = 16;
pub const MAX_LAYER_WIDTH: usize = 16;

/// Step indices in declaration order within each parallel layer.
#[derive(Debug)]
pub struct Graph {
    pub layers: Vec<Vec<usize>>,
    pub(crate) dependencies: Vec<BTreeSet<usize>>,
    stream_sources: Vec<Option<usize>>,
}

pub(crate) struct ExecutionPlan {
    pub groups: Vec<Vec<(usize, usize)>>,
    pub live_sources: Vec<Option<usize>>,
}

impl Graph {
    pub(crate) fn execution_plan(&self, workflow: &Workflow, no_cache: bool) -> ExecutionPlan {
        let mut groups = Vec::<Vec<_>>::new();
        let mut group_of = vec![0; workflow.steps.len()];
        let mut live_sources = vec![None; workflow.steps.len()];
        for (layer, indices) in self.layers.iter().enumerate() {
            for &index in indices {
                // Cache keys and retries need complete stdin. A retrying
                // producer only exposes the output of its successful attempt.
                let source = self.stream_sources[index].filter(|&source| {
                    matches!(&workflow.steps[index].kind, StepKind::Command(step) if step.pipe_mode == PipeMode::Concurrent && !step.retries() && (no_cache || !step.cache))
                        && matches!(&workflow.steps[source].kind, StepKind::Command(step) if !step.retries())
                });
                let group = self.dependencies[index]
                    .iter()
                    .map(|&dependency| {
                        group_of[dependency] + usize::from(Some(dependency) != source)
                    })
                    .max()
                    .unwrap_or(0);
                group_of[index] = group;
                live_sources[index] = source.filter(|&source| group_of[source] == group);
                groups.resize_with(groups.len().max(group + 1), Vec::new);
                groups[group].push((layer + 1, index));
            }
        }
        ExecutionPlan {
            groups,
            live_sources,
        }
    }

    pub fn build(workflow: &Workflow) -> Result<Self, Error> {
        Self::build_with_force(workflow, false)
    }

    pub fn build_with_force(workflow: &Workflow, force: bool) -> Result<Self, Error> {
        if workflow.version != "1.0" {
            return Err(Error::InvalidWorkflow(format!(
                "unsupported workflow version {:?}",
                workflow.version
            )));
        }

        let mut indices = HashMap::with_capacity(workflow.steps.len());
        for (index, step) in workflow.steps.iter().enumerate() {
            if step.id.is_empty() || indices.insert(step.id.as_str(), index).is_some() {
                return Err(Error::InvalidWorkflow(format!(
                    "empty or duplicate step ID {:?}",
                    step.id
                )));
            }
        }

        let environment = template::environment();
        let mut dependencies = vec![BTreeSet::new(); workflow.steps.len()];
        let mut stream_sources = vec![None; workflow.steps.len()];
        for (index, step) in workflow.steps.iter().enumerate() {
            let location = format!("step {:?}", step.id);
            let templates = match &step.kind {
                StepKind::Command(command) => {
                    command.validate_options(&location)?;
                    if command.cmd.first().is_none_or(String::is_empty) {
                        return Err(Error::InvalidWorkflow(format!(
                            "{location} has an empty cmd"
                        )));
                    }
                    if let Some(source) = &command.pipe_from {
                        if command.stdin.is_some() {
                            return Err(Error::InvalidWorkflow(format!(
                                "{location} defines both stdin and pipe_from"
                            )));
                        }
                        let source = step_index(&indices, &location, source)?;
                        if !matches!(workflow.steps[source].kind, StepKind::Command(_)) {
                            return Err(Error::InvalidWorkflow(format!(
                                "{location} must pipe from a command step"
                            )));
                        }
                        stream_sources[index] = Some(source);
                    }
                    for path in &command.cache_files {
                        let location = format!("cache_files for {location}");
                        let template = environment.template_from_str(path).map_err(|source| {
                            Error::Template {
                                location: location.clone(),
                                source,
                            }
                        })?;
                        if template
                            .undeclared_variables(true)
                            .iter()
                            .any(|name| name != "inputs" && !name.starts_with("inputs."))
                        {
                            return Err(Error::InvalidWorkflow(format!(
                                "{location} may only reference inputs"
                            )));
                        }
                    }
                    command.cmd.iter().chain(&command.stdin).map(String::as_str)
                }
                StepKind::Switch(switch) => {
                    if switch.cases.is_empty() {
                        return Err(Error::InvalidWorkflow(format!("{location} has no cases")));
                    }
                    for target in switch.cases.values() {
                        let target = step_index(&indices, &location, target)?;
                        dependencies[target].insert(index);
                    }
                    add_template_dependencies(
                        &environment,
                        &switch.condition,
                        &location,
                        &indices,
                        workflow,
                        &mut dependencies[index],
                    )?;
                    continue;
                }
            };
            for source in templates {
                add_template_dependencies(
                    &environment,
                    source,
                    &location,
                    &indices,
                    workflow,
                    &mut dependencies[index],
                )?;
            }
            if let Some(source) = stream_sources[index] {
                if !dependencies[index].insert(source) {
                    // A template dependency on the same producer is still a barrier.
                    stream_sources[index] = None;
                }
            }
        }

        if let Some(output) = &workflow.output {
            add_template_dependencies(
                &environment,
                output,
                "workflow output",
                &indices,
                workflow,
                &mut BTreeSet::new(),
            )?;
        }

        let layers = sort_layers(workflow, &dependencies)?;
        let steps = workflow.steps.len();
        let depth = layers.len();
        let max_width = layers.iter().map(Vec::len).max().unwrap_or(0);
        if steps > MAX_STEPS || depth > MAX_DEPTH || max_width > MAX_LAYER_WIDTH {
            if !force {
                return Err(Error::GraphLimitExceeded {
                    steps,
                    depth,
                    max_width,
                    max_steps: MAX_STEPS,
                    max_depth: MAX_DEPTH,
                    max_layer_width: MAX_LAYER_WIDTH,
                });
            }
            eprintln!(
                "WARNING: Graph safety limits bypassed with --force (steps: {steps}, depth: {depth}, max_width: {max_width})"
            );
        }
        let graph = Self {
            layers,
            dependencies,
            stream_sources,
        };
        let plan = graph.execution_plan(workflow, false);
        for (index, step) in workflow.steps.iter().enumerate() {
            if let StepKind::Command(command) = &step.kind {
                if let Some(source) = &command.pipe_from {
                    let source_index = indices[source.as_str()];
                    if !captures_stdout(workflow, source_index)
                        && plan.live_sources[index] != Some(source_index)
                    {
                        return Err(Error::InvalidWorkflow(format!(
                            "step {:?} needs a live pipe from {source:?} because it uses capture_stdout: false",
                            step.id
                        )));
                    }
                }
            }
        }
        Ok(graph)
    }
}

fn step_index(indices: &HashMap<&str, usize>, location: &str, id: &str) -> Result<usize, Error> {
    indices
        .get(id)
        .copied()
        .ok_or_else(|| Error::InvalidWorkflow(format!("{location} references unknown step {id:?}")))
}

fn add_template_dependencies(
    environment: &Environment<'_>,
    source: &str,
    location: &str,
    indices: &HashMap<&str, usize>,
    workflow: &Workflow,
    dependencies: &mut BTreeSet<usize>,
) -> Result<(), Error> {
    let template = environment
        .template_from_str(source)
        .map_err(|source| Error::Template {
            location: location.to_owned(),
            source,
        })?;
    let references: BTreeSet<_> = template.undeclared_variables(true).into_iter().collect();
    for reference in references {
        if reference == "steps" {
            return Err(Error::InvalidWorkflow(format!(
                "{location} must reference steps by a static name, such as steps.example.stdout"
            )));
        }
        if let Some(path) = reference.strip_prefix("steps.") {
            let (id, field) = path.split_once('.').unwrap_or((path, ""));
            let index = step_index(indices, location, id)?;
            if !captures_stdout(workflow, index) && !matches!(field, "stderr" | "exit_code") {
                return Err(Error::InvalidWorkflow(format!(
                    "{location} references {reference:?}, but step {id:?} uses capture_stdout: false"
                )));
            }
            dependencies.insert(index);
        }
    }
    Ok(())
}

fn captures_stdout(workflow: &Workflow, index: usize) -> bool {
    !matches!(&workflow.steps[index].kind, StepKind::Command(step) if !step.capture_stdout)
}

fn sort_layers(
    workflow: &Workflow,
    dependencies: &[BTreeSet<usize>],
) -> Result<Vec<Vec<usize>>, Error> {
    let mut indegrees: Vec<_> = dependencies.iter().map(BTreeSet::len).collect();
    let mut successors = vec![Vec::new(); dependencies.len()];
    for (index, sources) in dependencies.iter().enumerate() {
        for &source in sources {
            successors[source].push(index);
        }
    }

    let mut ready: Vec<_> = (0..indegrees.len())
        .filter(|&index| indegrees[index] == 0)
        .collect();
    let mut layers = Vec::new();
    while !ready.is_empty() {
        let mut next = Vec::new();
        for &source in &ready {
            for &target in &successors[source] {
                indegrees[target] -= 1;
                if indegrees[target] == 0 {
                    next.push(target);
                }
            }
        }
        next.sort_unstable();
        layers.push(ready);
        ready = next;
    }

    let blocked: Vec<_> = indegrees
        .iter()
        .enumerate()
        .filter(|&(_, &degree)| degree != 0)
        .map(|(index, _)| workflow.steps[index].id.clone())
        .collect();
    if blocked.is_empty() {
        Ok(layers)
    } else {
        Err(Error::Cycle(blocked))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::schema::readme_workflow;

    #[test]
    fn groups_readme_steps_into_parallel_layers() {
        let workflow = readme_workflow();
        let graph = Graph::build(&workflow).unwrap();
        assert_eq!(graph.layers, [vec![0, 1], vec![2], vec![3], vec![4, 5]]);
        let plan = graph.execution_plan(&workflow, false);
        assert_eq!(plan.live_sources[4], None);
        assert_eq!(plan.groups.last().unwrap(), &[(4, 4), (4, 5)]);
    }

    #[test]
    fn deduplicates_dependencies_from_pipes_and_templates() {
        let workflow = Workflow::from_yaml(
            r#"
version: "1.0"
name: pipe
steps:
  - id: consumer
    cmd: [cat, "{{ steps.producer.stdout }}", "{{ steps.producer.stdout }}"]
    pipe_from: producer
  - id: producer
    cmd: [echo]
"#,
        )
        .unwrap();
        let graph = Graph::build(&workflow).unwrap();
        assert_eq!(graph.layers, [vec![1], vec![0]]);
        assert_eq!(
            graph.execution_plan(&workflow, false).groups,
            [vec![(1, 1)], vec![(2, 0)]]
        );
    }

    #[test]
    fn detects_cycle_through_a_pipe_and_a_template() {
        let workflow = Workflow::from_yaml(
            r#"
version: "1.0"
name: cycle
steps:
  - id: first
    cmd: [cat]
    pipe_from: second
  - id: second
    cmd: [cat]
    stdin: "{{ steps.first.stdout }}"
"#,
        )
        .unwrap();
        assert!(matches!(
            Graph::build(&workflow),
            Err(Error::Cycle(ids)) if ids == ["first", "second"]
        ));
    }
}
