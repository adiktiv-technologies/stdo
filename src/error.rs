use std::{io, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read workflow {path:?}: {source}")]
    ReadWorkflow { path: PathBuf, source: io::Error },
    #[error("invalid workflow YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("{0}")]
    InvalidWorkflow(String),
    #[error("workflow contains a dependency cycle; blocked steps: {}", .0.join(", "))]
    Cycle(Vec<String>),
    #[error("invalid template in {location}: {source}")]
    Template {
        location: String,
        source: minijinja::Error,
    },
    #[error("required executables unavailable: {}", .0.join(", "))]
    MissingExecutables(Vec<String>),
}
