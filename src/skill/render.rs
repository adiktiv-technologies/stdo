use crate::skill::{
    exit::{usage, Result},
    introspect::parser::SkillTree,
};
pub fn render(
    tree: &SkillTree,
    target: &str,
    name: &str,
    version: Option<&str>,
    limit: usize,
    native_scope: Option<&[String]>,
) -> Result<(String, bool)> {
    let root = &tree[&Vec::new()];
    let synopsis = if root.synopsis.is_empty() {
        target
    } else {
        &root.synopsis
    };
    let desc = format!(
        "Use {target} for {}",
        synopsis
            .split_whitespace()
            .take(16)
            .collect::<Vec<_>>()
            .join(" ")
    );
    let desc = match native_scope {
        Some([]) => super::stdo::DESCRIPTION.to_owned(),
        Some(_) if !root.description.is_empty() => root.description.clone(),
        _ => desc,
    };
    let mut headings = vec![
        "---".to_string(),
        format!("name: {}", serde_json::to_string(name).unwrap()),
        format!("description: {}", serde_json::to_string(&desc).unwrap()),
        "x-generated-by: skill".into(),
        "---".into(),
        "## Synopsis".into(),
        format!("`{synopsis}`"),
    ];
    if let Some(version) = version {
        headings.insert(
            3,
            format!("x-cli-version: {}", serde_json::to_string(version).unwrap()),
        );
    }
    let required = headings.len() + 2;
    if limit != 0 && limit < required {
        return Err(usage(format!(
            "line limit must allow at least {required} lines"
        )));
    }
    let mut rows = Vec::new();
    // Keep priority is the reverse of the removal order. A stable sort resolves ties.
    for (path, n) in tree {
        let label = path.join(" ");
        if !path.is_empty() {
            let usage = if native_scope.is_some() && !n.synopsis.is_empty() {
                &n.synopsis
            } else {
                &label
            };
            rows.push((0, path.len(), 1, format!("- `{usage}`")));
        }
        for flag in &n.flags {
            let primary = flag.starts_with('-') && !flag.starts_with("--")
                || flag.starts_with("--help")
                || flag.starts_with("--version");
            rows.push((
                if primary { 0 } else { 1 },
                path.len(),
                2,
                format!(
                    "- `{}{}`",
                    if label.is_empty() {
                        String::new()
                    } else {
                        format!("{label} ")
                    },
                    flag
                ),
            ));
        }
        if !n.description.is_empty() {
            rows.push((
                2,
                path.len(),
                if path.is_empty() { 0 } else { 1 },
                format!(
                    "{}{}",
                    if label.is_empty() {
                        String::new()
                    } else {
                        format!("`{label}` — ")
                    },
                    n.description
                ),
            ));
        }
        for ex in &n.examples {
            rows.push((3, path.len(), 0, format!("Example: `{ex}`")));
        }
    }
    if let Some(scope) = native_scope {
        if (scope.is_empty() && tree.keys().any(|p| p.first().is_some_and(|s| s == "run")))
            || matches!(scope, [cmd] if matches!(cmd.as_str(),"doctor"|"run"|"serve"|"top"|"bench"))
        {
            for paragraph in super::stdo::GUIDE.trim().split("\n\n") {
                if paragraph == "<!-- workflow-example -->" {
                    rows.push((3, 0, 3, super::stdo::WORKFLOW_EXAMPLE.to_owned()));
                } else {
                    rows.push((2, 0, 3, paragraph.to_owned()));
                }
            }
        }
        for path in tree.keys() {
            let mut parts = scope.iter().chain(path);
            if let (Some(name), None) = (parts.next(), parts.next()) {
                if let Some(hint) = super::stdo::command(name) {
                    rows.push((2, 0, 3, format!("- **{name}** — {hint}")));
                }
            }
        }
    }
    let trimmed =
        limit != 0 && rows.iter().map(|r| r.3.lines().count()).sum::<usize>() + required > limit;
    if trimmed {
        let mut order: Vec<_> = (0..rows.len()).collect();
        order.sort_by_key(|&i| (rows[i].0, rows[i].1));
        let mut remaining = limit - required;
        let mut keep = vec![false; rows.len()];
        for i in order {
            let lines = rows[i].3.lines().count();
            if lines <= remaining {
                keep[i] = true;
                remaining -= lines;
            }
        }
        rows = rows
            .into_iter()
            .zip(keep)
            .filter_map(|(row, keep)| keep.then_some(row))
            .collect();
    }
    for section in 0..4 {
        match section {
            1 => headings.push("## Subcommands".into()),
            2 => headings.push("## Flags".into()),
            _ => {}
        }
        headings.extend(rows.iter().filter(|r| r.2 == section).map(|r| r.3.clone()));
    }
    Ok((headings.join("\n") + "\n", trimmed))
}
