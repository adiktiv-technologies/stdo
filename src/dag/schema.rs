use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub version: String,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, Input>,
    pub steps: Vec<Step>,
    pub output: Option<String>,
}

impl Workflow {
    pub fn from_yaml(yaml: &str) -> Result<Self, Error> {
        Ok(serde_yaml::from_str(yaml)?)
    }

    pub fn read(path: &Path) -> Result<Self, Error> {
        Self::from_yaml(&Self::read_yaml(path)?)
    }

    pub(crate) fn read_yaml(path: &Path) -> Result<String, Error> {
        std::fs::read_to_string(path).map_err(|source| Error::ReadWorkflow {
            path: path.to_owned(),
            source,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    #[serde(rename = "type")]
    pub kind: InputKind,
    pub default: Option<Value>,
    pub description: Option<String>,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InputKind {
    String,
    Path,
    Enum,
    Number,
    Boolean,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Step {
    pub id: String,
    pub name: Option<String>,
    #[serde(flatten)]
    pub kind: StepKind,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum StepKind {
    Switch(SwitchStep),
    Command(CommandStep),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SwitchStep {
    #[serde(rename = "type")]
    _kind: SwitchKind,
    pub condition: String,
    pub cases: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum SwitchKind {
    Switch,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandStep {
    pub cmd: Vec<String>,
    pub stdin: Option<String>,
    pub pipe_from: Option<String>,
    #[serde(default, skip_serializing_if = "PipeMode::is_concurrent")]
    pub pipe_mode: PipeMode,
    #[serde(default = "capture_by_default", skip_serializing_if = "is_captured")]
    pub capture_stdout: bool,
    #[serde(default)]
    pub ignore_exit_code: bool,
    #[serde(default)]
    pub cache: bool,
    pub parse_stdout: Option<StdoutFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_env: Vec<String>,
}

fn capture_by_default() -> bool {
    true
}

fn is_captured(capture: &bool) -> bool {
    *capture
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PipeMode {
    #[default]
    Concurrent,
    Atomic,
}

impl PipeMode {
    fn is_concurrent(&self) -> bool {
        *self == Self::Concurrent
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    pub attempts: u32,
    #[serde(default)]
    pub backoff_ms: u64,
}

impl CommandStep {
    pub(crate) fn retries(&self) -> bool {
        self.retry.is_some_and(|retry| retry.attempts > 1)
    }

    pub(crate) fn validate_options(&self, location: &str) -> Result<(), Error> {
        let invalid = |message| Error::InvalidWorkflow(format!("{location}: {message}"));
        if self.pipe_mode == PipeMode::Atomic && self.pipe_from.is_none() {
            return Err(invalid("pipe_mode: atomic requires pipe_from"));
        }
        if !self.capture_stdout && (self.cache || self.parse_stdout == Some(StdoutFormat::Json)) {
            return Err(invalid(
                "capture_stdout: false cannot use cache or parse_stdout: json",
            ));
        }
        if self.timeout_ms == Some(0) {
            return Err(invalid("timeout_ms must be greater than 0"));
        }
        if let Some(retry) = self.retry {
            if !(1..=10).contains(&retry.attempts) {
                return Err(invalid("retry.attempts must be between 1 and 10"));
            }
            if retry.backoff_ms > 60_000 {
                return Err(invalid("retry.backoff_ms must be at most 60000"));
            }
        }
        if self
            .cache_env
            .iter()
            .any(|name| name.is_empty() || name.contains(['=', '\0']))
        {
            return Err(invalid(
                "cache_env contains an invalid environment variable name",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StdoutFormat {
    Json,
    Jsonl,
}

#[cfg(test)]
pub(crate) fn readme_workflow() -> Workflow {
    let yaml = include_str!("../../README.md")
        .split_once("```yaml\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    Workflow::from_yaml(yaml).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_offline_code_triage_from_readme() {
        let workflow = readme_workflow();
        assert_eq!(workflow.version, "1.0");
        assert_eq!(workflow.name, "offline-code-triage");
        assert_eq!(workflow.steps.len(), 6);
        assert_eq!(
            workflow.inputs["severity"].values,
            ["LOW", "HIGH", "CRITICAL"]
        );
        assert_eq!(
            workflow.inputs["severity"].default,
            Some(Value::from("HIGH"))
        );

        let StepKind::Command(scan) = &workflow.steps[0].kind else {
            panic!("expected a command step");
        };
        assert_eq!(scan.cmd[0], "rg");
        assert!(scan.ignore_exit_code);

        let StepKind::Command(audit) = &workflow.steps[2].kind else {
            panic!("expected a command step");
        };
        assert!(audit.cache);
        assert_eq!(audit.parse_stdout, Some(StdoutFormat::Json));

        let StepKind::Switch(gate) = &workflow.steps[3].kind else {
            panic!("expected a switch step");
        };
        assert_eq!(gate.cases["true"], "record_incident");
        assert_eq!(gate.cases["false"], "approve_release");
        assert_eq!(
            workflow.output.as_deref(),
            Some("{{ steps.local_llm_audit.json.summary }}")
        );
    }
}
