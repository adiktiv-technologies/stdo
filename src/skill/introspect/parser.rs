use std::collections::{BTreeMap, BTreeSet};
#[derive(Default, Clone)]
pub struct Node {
    pub synopsis: String,
    pub description: String,
    pub children: BTreeMap<String, String>,
    pub flags: BTreeSet<String>,
    pub examples: Vec<String>,
}
pub type SkillTree = BTreeMap<Vec<String>, Node>;
pub fn token(word: &str) -> bool {
    !word.is_empty()
        && word.as_bytes()[0].is_ascii_alphanumeric()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub fn signature(s: &str) -> bool {
    s.lines().any(|l| {
        let t = l.trim().to_ascii_lowercase();
        t == "usage"
            || t == "synopsis"
            || ["usage:", "synopsis:", "options:", "commands:"]
                .iter()
                .any(|p| t.starts_with(p))
    })
}
pub fn parse(text: &str) -> Node {
    let mut n = Node::default();
    let mut section = "";
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let low = l.to_ascii_lowercase();
        if let Some(rest) = low.strip_prefix("usage:") {
            n.synopsis = l[l.len() - rest.len()..].trim().into();
            section = "usage";
            continue;
        }
        if matches!(low.as_str(), "usage" | "synopsis" | "synopsis:") {
            section = "usage";
            continue;
        }
        if low == "commands"
            || low.ends_with(" commands")
            || low.ends_with("commands:")
            || low == "subcommands"
        {
            section = "commands";
            continue;
        }
        if low == "options"
            || low.ends_with("options:")
            || low.trim_end_matches(':').ends_with("flags")
        {
            section = "flags";
            continue;
        }
        if low == "examples:" || low == "examples" {
            section = "examples";
            continue;
        }
        if l.ends_with(':') || l == "DESCRIPTION" {
            section = "other";
            continue;
        }
        if l.starts_with('-') {
            n.flags.insert(l.to_owned());
            continue;
        }
        match section {
            "usage" if n.synopsis.is_empty() => n.synopsis = l.into(),
            "commands" => {
                let mut parts = l.splitn(2, char::is_whitespace);
                let word = parts.next().unwrap_or("").trim_end_matches(':');
                if token(word) && word != "help" {
                    n.children
                        .insert(word.into(), parts.next().unwrap_or("").trim().into());
                }
            }
            "examples" => n.examples.push(l.into()),
            "" | "other" if n.description.is_empty() => n.description = l.into(),
            _ => {}
        }
    }
    n
}
