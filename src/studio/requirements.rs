use crate::{
    dag::schema::{InputKind, StepKind, Workflow},
    doctor::resolve_executable,
    engine::template,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn summary(workflow: &Workflow, overrides: &[String]) -> Value {
    let mut inputs: BTreeMap<String, Value> = workflow
        .inputs
        .iter()
        .filter_map(|(name, input)| input.default.clone().map(|value| (name.clone(), value)))
        .collect();
    for argument in overrides {
        if let Some((name, value)) = argument.split_once('=') {
            inputs.insert(name.to_owned(), Value::String(value.to_owned()));
        }
    }
    let context = json!({"inputs": inputs});
    let mut binaries = BTreeSet::new();
    let mut models = BTreeSet::new();
    for step in &workflow.steps {
        let StepKind::Command(command) = &step.kind else {
            continue;
        };
        let args: Vec<_> = command
            .cmd
            .iter()
            .map(|arg| template::render(arg, &context).unwrap_or_else(|_| arg.clone()))
            .collect();
        if let Some(program) = args.first().filter(|arg| !arg.is_empty()) {
            binaries.insert(program.clone());
            let basename = std::path::Path::new(program)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(program);
            if basename == "ollama" && args.get(1).is_some_and(|arg| arg == "run") {
                if let Some(model) = args.get(2) {
                    models.insert(model.clone());
                }
            }
            if matches!(basename, "sh" | "bash" | "zsh")
                && args.get(1).is_some_and(|arg| arg == "-c")
            {
                if let Some(script) = args.get(2) {
                    let tokens: BTreeSet<_> = script
                        .split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '_' | '-'))
                        .collect();
                    for tool in [
                        "ffmpeg", "jq", "rg", "sqlite3", "curl", "ollama", "find", "grep", "wc",
                        "cat", "mv", "rm", "tr",
                    ] {
                        if tokens.contains(tool) {
                            binaries.insert(tool.to_owned());
                        }
                    }
                }
            }
        }
    }
    for name in ["llm_model", "tts_model"] {
        if let Some(model) = inputs
            .get(name)
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
        {
            models.insert(model.to_owned());
        }
    }
    let binaries: Vec<_> = binaries
        .into_iter()
        .map(|name| {
            let path = resolve_executable(&name).and_then(|path| std::fs::canonicalize(path).ok());
            json!({"name":name,"installed":path.is_some(),"path":path})
        })
        .collect();
    let inputs: Vec<_> = workflow
        .inputs
        .iter()
        .filter_map(|(name, input)| {
            let value = inputs.get(name);
            let required = value.is_none();
            let file = matches!(input.kind, InputKind::Path) || name.ends_with("_video");
            if !required && !file {
                return None;
            }
            let path = value.and_then(Value::as_str).unwrap_or("");
            let output = name.starts_with("output");
            let status = if required {
                "required"
            } else if path.is_empty() {
                "optional"
            } else if output {
                "output"
            } else if std::path::Path::new(path).exists() {
                "exists"
            } else {
                "missing"
            };
            Some(json!({"name":name,"value":value,"status":status}))
        })
        .collect();
    json!({"binaries":binaries,"models":models,"inputs":inputs,
        "note":"Shell tools are detected heuristically. Model names are hints; local model availability is not probed."})
}
