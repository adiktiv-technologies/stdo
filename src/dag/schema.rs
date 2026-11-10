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
    #[serde(default)]
    pub ignore_exit_code: bool,
    #[serde(default)]
    pub cache: bool,
    pub parse_stdout: Option<StdoutFormat>,
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
