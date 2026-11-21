use crate::skill::{
    exit::{data, usage, Result},
    introspect::parser::SkillTree,
};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Read},
};
fn patterns(values: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for v in values {
        b.add(Glob::new(v).map_err(|e| usage(e.to_string()))?);
    }
    b.build().map_err(|e| usage(e.to_string()))
}
pub fn apply(
    tree: &mut SkillTree,
    prefix: &[String],
    includes: &[String],
    excludes: &[String],
) -> Result<bool> {
    let inc = patterns(includes)?;
    let exc = patterns(excludes)?;
    let mut bytes = Vec::new();
    if !io::stdin().is_terminal() {
        io::stdin().read_to_end(&mut bytes)?;
    }
    let mut allowed = BTreeSet::new();
    for line in std::str::from_utf8(&bytes)
        .map_err(|_| data("stdin must be UTF-8"))?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let components: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        let relative = components
            .strip_prefix(prefix)
            .ok_or_else(|| data(format!("invalid path: {line}")))?;
        if relative.is_empty() || !tree.contains_key(relative) {
            return Err(data(format!("unknown path: {line}")));
        }
        allowed.insert(relative.to_vec());
    }
    if !bytes.is_empty() && allowed.is_empty() {
        return Err(data("empty whitelist"));
    }
    tree.retain(|path, node| {
        let full = prefix
            .iter()
            .chain(path)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let selected = includes.is_empty() || inc.is_match(&full);
        if !path.is_empty()
            && (exc.is_match(&full) || (!allowed.is_empty() && !allowed.contains(path)))
        {
            return false;
        }
        node.flags.retain(|line| {
            let flags = line
                .split_whitespace()
                .take_while(|s| s.starts_with('-'))
                .map(|s| s.trim_end_matches(',').split('=').next().unwrap_or(s))
                .collect::<Vec<_>>();
            let matches = |set: &GlobSet| {
                flags
                    .iter()
                    .any(|s| set.is_match(s) || set.is_match(format!("{full} {s}").trim()))
            };
            !matches(&exc) && (selected || matches(&inc))
        });
        path.is_empty() || selected || !node.flags.is_empty()
    });
    Ok(!bytes.is_empty())
}
