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
    let mut binary_sources: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let mut model_references: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
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
            binary_sources
                .entry(program.clone())
                .or_default()
                .insert("step cmd");
            let basename = std::path::Path::new(program)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(program);
            if basename == "ollama" && args.get(1).is_some_and(|arg| arg == "run") {
                if let Some(model) = args.get(2) {
                    models.insert(model.clone());
                    model_references
                        .entry(model.clone())
                        .or_default()
                        .insert(step.id.clone());
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
                            binary_sources
                                .entry(tool.to_owned())
                                .or_default()
                                .insert("shell script");
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
            model_references
                .entry(model.to_owned())
                .or_default()
                .insert(format!("input {name}"));
        }
    }
    let binaries: Vec<_> = binaries
        .into_iter()
        .map(|name| {
            let path = resolve_executable(&name).and_then(|path| std::fs::canonicalize(path).ok());
            json!({"name":name,"installed":path.is_some(),"path":path,"sources":binary_sources.get(&name)})
        })
        .collect();
    let inputs: Vec<_> = workflow
        .inputs
        .iter()
        .map(|(name, input)| {
            let value = inputs.get(name);
            let required = value.is_none();
            let file = matches!(input.kind, InputKind::Path) || name.ends_with("_video");
            let path = value.and_then(Value::as_str).unwrap_or("");
            let output = name.starts_with("output");
            let status = if required {
                "required"
            } else if path.is_empty() {
                "optional"
            } else if output {
                "output"
            } else if !file {
                "optional"
            } else if std::path::Path::new(path).exists() {
                "exists"
            } else {
                "missing"
            };
            let role = if output {
                "Output artifact"
            } else if input.default.is_none() {
                "Required"
            } else {
                "Optional"
            };
            json!({"name":name,"value":value,"status":status,"type":input.kind,"role":role})
        })
        .collect();
    let model_details: Vec<_> = models
        .iter()
        .map(|name| json!({"name":name,"references":model_references.get(name)}))
        .collect();
    json!({"binaries":binaries,"models":models,"model_details":model_details,"inputs":inputs,
        "note":"Shell tools are detected heuristically. Model names are hints; local model availability is not probed."})
}

pub(super) fn recursive(
    workflow: &Workflow,
    overrides: &[String],
    root: &std::path::Path,
    source: Option<&std::path::Path>,
) -> Value {
    let mut result = summary(workflow, overrides);
    match crate::doctor::subworkflows::collect(workflow, root, source, overrides, false) {
        Ok(children) => {
            let mut paths = BTreeSet::new();
            for child in children {
                let path = child
                    .path
                    .strip_prefix(root)
                    .unwrap_or(&child.path)
                    .display()
                    .to_string();
                paths.insert(path.clone());
                let nested = summary(&child.workflow, &child.inputs);
                for entry in nested["binaries"].as_array().expect("binaries") {
                    let entries = result["binaries"].as_array_mut().expect("binaries");
                    let origin = json!(format!("sub-workflow ({path})"));
                    if let Some(existing) = entries
                        .iter_mut()
                        .find(|item| item["name"] == entry["name"])
                    {
                        let sources = existing["sources"].as_array_mut().expect("sources");
                        if !sources.contains(&origin) {
                            sources.push(origin);
                        }
                    } else {
                        let mut entry = entry.clone();
                        entry["sources"] = json!([origin]);
                        entries.push(entry);
                    }
                }
                for entry in nested["model_details"].as_array().expect("model_details") {
                    let entries = result["model_details"]
                        .as_array_mut()
                        .expect("model_details");
                    let origin = json!(format!("sub-workflow ({path})"));
                    if let Some(existing) = entries
                        .iter_mut()
                        .find(|item| item["name"] == entry["name"])
                    {
                        let refs = existing["references"].as_array_mut().expect("references");
                        if !refs.contains(&origin) {
                            refs.push(origin);
                        }
                    } else {
                        entries.push(json!({"name":entry["name"],"references":[origin]}));
                    }
                }
            }
            result["subworkflows"] = json!(paths);
        }
        Err(error) => result["error"] = json!(error.to_string()),
    }
    result["binaries"]
        .as_array_mut()
        .expect("binaries")
        .sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut models: Vec<_> = result["model_details"]
        .as_array()
        .expect("models")
        .iter()
        .map(|item| item["name"].clone())
        .collect();
    models.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    result["models"] = json!(models);
    result
}
