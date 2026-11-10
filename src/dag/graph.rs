use std::collections::{BTreeSet, HashMap};

use minijinja::Environment;

use crate::{engine::template, error::Error};

use super::schema::{StepKind, Workflow};

/// Step indices in declaration order within each parallel layer.
#[derive(Debug)]
pub struct Graph {
    pub layers: Vec<Vec<usize>>,
}

impl Graph {
    pub fn build(workflow: &Workflow) -> Result<Self, Error> {
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
        for (index, step) in workflow.steps.iter().enumerate() {
            let location = format!("step {:?}", step.id);
            let templates = match &step.kind {
                StepKind::Command(command) => {
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
                        dependencies[index].insert(source);
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
                    &mut dependencies[index],
                )?;
            }
        }

        if let Some(output) = &workflow.output {
            add_template_dependencies(
                &environment,
                output,
                "workflow output",
                &indices,
                &mut BTreeSet::new(),
            )?;
        }

        sort_layers(workflow, dependencies).map(|layers| Self { layers })
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
            let id = path
                .split('.')
                .next()
                .expect("split yields at least one item");
            dependencies.insert(step_index(indices, location, id)?);
        }
    }
    Ok(())
}

fn sort_layers(
    workflow: &Workflow,
    dependencies: Vec<BTreeSet<usize>>,
) -> Result<Vec<Vec<usize>>, Error> {
    let mut indegrees: Vec<_> = dependencies.iter().map(BTreeSet::len).collect();
    let mut successors = vec![Vec::new(); dependencies.len()];
    for (index, sources) in dependencies.into_iter().enumerate() {
        for source in sources {
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
        let graph = Graph::build(&readme_workflow()).unwrap();
        assert_eq!(graph.layers, [vec![0, 1], vec![2], vec![3], vec![4, 5]]);
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
        assert_eq!(Graph::build(&workflow).unwrap().layers, [vec![1], vec![0]]);
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
