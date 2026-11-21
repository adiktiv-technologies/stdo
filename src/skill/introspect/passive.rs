//! Only read declarative metadata. Never source installed completion scripts.
use super::{
    active,
    parser::{self, Node},
};
use crate::skill::exit::Result;
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};
fn paths(name: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = env::var_os("XDG_DATA_HOME") {
        roots.push(root.into())
    } else if let Some(home) = env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share"))
    }
    roots.extend(env::split_paths(
        &env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
    ));
    let mut paths = Vec::new();
    for pattern in [
        "bash-completion/completions/",
        "zsh/site-functions/_",
        "fish/vendor_completions.d/",
        "fish/completions/",
    ] {
        for root in &roots {
            let suffix = if pattern.starts_with("fish") {
                ".fish"
            } else {
                ""
            };
            paths.push(root.join(format!("{pattern}{name}{suffix}")));
        }
    }
    paths
}
fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split_whitespace().filter(|w| parser::token(w))
}
fn quoted(s: &str) -> Option<&str> {
    let s = s.trim_start();
    let q = s.chars().next()?;
    if !matches!(q, '\'' | '"') {
        return None;
    }
    s[1..].split_once(q).map(|(value, _)| value)
}
fn completion(text: &str, name: &str) -> Option<Node> {
    let mut children = BTreeMap::new();
    for l in text.lines().map(str::trim) {
        let values = if l.starts_with("complete -W ") && l.split_whitespace().last() == Some(name) {
            quoted(&l["complete -W ".len()..])
        } else if l.starts_with(&format!("complete -c {name} "))
            && (!l.contains(" -n ") || l.contains("__fish_use_subcommand"))
        {
            l.split_once(" -a ")
                .and_then(|(_, s)| quoted(s).or_else(|| s.split_whitespace().next()))
        } else {
            None
        };
        if let Some(values) = values {
            children.extend(words(values).map(|w| (w.into(), String::new())));
        }
        if let Some(value) = quoted(l) {
            if let Some((word, desc)) = value.split_once(':') {
                if parser::token(word) && text.contains("_describe") {
                    children.insert(word.into(), desc.into());
                }
            }
        }
    }
    (!children.is_empty()).then(|| Node {
        synopsis: name.into(),
        children,
        ..Node::default()
    })
}
pub fn read(name: &str, ms: u64) -> Result<Option<Node>> {
    for path in paths(name) {
        if let Ok(s) = fs::read_to_string(path) {
            if let Some(node) = completion(&s, name) {
                return Ok(Some(node));
            }
        }
    }
    let Some(man) = super::executable("man") else {
        return Ok(None);
    };
    let located = active::capture(&man, &["-w", name], ms)?;
    if located.code != Some(0) {
        return Ok(None);
    }
    let output = String::from_utf8_lossy(&located.stdout);
    let Some(first) = output.lines().next() else {
        return Ok(None);
    };
    let path = Path::new(first.trim());
    if !path.is_absolute() || !path.is_file() {
        return Ok(None);
    }
    let bytes = if path.extension().is_some_and(|s| s == "gz") {
        let Some(gzip) = super::executable("gzip") else {
            return Ok(None);
        };
        let c = active::capture(&gzip, &[std::ffi::OsStr::new("-dc"), path.as_os_str()], ms)?;
        if c.code != Some(0) {
            return Ok(None);
        }
        c.stdout
    } else {
        fs::read(path)?
    };
    let mut plain = String::new();
    for l in String::from_utf8_lossy(&bytes).lines() {
        let l = l
            .trim()
            .trim_start_matches(".SH ")
            .trim_start_matches(".Nm ")
            .trim_start_matches(".B ")
            .trim_start_matches(".BR ")
            .trim_matches('"');
        if !l.starts_with('.') {
            plain.push_str(
                &l.replace("\\-", "-")
                    .replace("\\fB", "")
                    .replace("\\fR", "")
                    .replace("\\fP", ""),
            );
            plain.push('\n');
        }
    }
    Ok(parser::signature(&plain).then(|| parser::parse(&plain)))
}
