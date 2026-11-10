use std::{fmt, io, path::PathBuf};

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
    #[error("graph safety limits exceeded (steps: {steps}/{max_steps}, depth: {depth}/{max_depth}, max_width: {max_width}/{max_layer_width}); pass --force to bypass")]
    GraphLimitExceeded {
        steps: usize,
        depth: usize,
        max_width: usize,
        max_steps: usize,
        max_depth: usize,
        max_layer_width: usize,
    },
    #[error("step {step:?} exceeded the memory limit of {limit_bytes} bytes")]
    MemoryLimitExceeded { step: String, limit_bytes: usize },
    #[error("process I/O failed for step {step:?}: {source}")]
    Process { step: String, source: io::Error },
    #[error("step {step:?} failed with {status}{}", Diagnostic(.stderr))]
    StepFailed {
        step: String,
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error("step {step:?} produced invalid JSON: {source}")]
    Json {
        step: String,
        source: serde_json::Error,
    },
    #[error("could not write workflow output: {0}")]
    WriteOutput(io::Error),
}

struct Diagnostic<'a>(&'a str);

impl fmt::Display for Diagnostic<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            Ok(())
        } else {
            write!(formatter, "\n{}", self.0)
        }
    }
}
