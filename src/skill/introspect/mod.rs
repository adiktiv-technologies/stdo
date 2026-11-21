pub mod active;
pub mod parser;
mod passive;
use crate::skill::exit::{data, Code, Error, Result};
use parser::{Node, SkillTree};
use std::{
    collections::VecDeque,
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
pub fn executable(name: &str) -> Option<PathBuf> {
    let valid = |p: &Path| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        return valid(Path::new(name)).then(|| PathBuf::from(name));
    }
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| valid(p))
}
pub fn discover(
    target: &[String],
    depth: usize,
    ms: u64,
    verbose: bool,
    want_version: bool,
) -> Result<(SkillTree, Option<String>)> {
    let bin = executable(&target[0]).ok_or_else(|| {
        Error(
            Code::Missing,
            format!("executable not found: {}", target[0]),
        )
    })?;
    let name = bin
        .file_name()
        .and_then(|p| p.to_str())
        .ok_or_else(|| data("invalid program name"))?;
    let active_node = |path: &[String]| -> Result<Node> {
        if verbose {
            eprintln!("skill: inspecting {name} {}", path.join(" "))
        }
        Ok(parser::parse(&active::help(&bin, path, ms)?))
    };
    let passive = passive::read(name, ms)?;
    let mut verified = false;
    let mut root = match passive {
        Some(n) => n,
        None => {
            verified = true;
            active_node(&[])?
        }
    };
    let advertises_version = root.flags.iter().any(|line| {
        line.split_whitespace()
            .take_while(|word| word.starts_with('-'))
            .any(|word| word.trim_end_matches(',') == "--version")
    });
    let version = if want_version && advertises_version {
        let reply = active::capture(&bin, &["--version"], ms)?;
        let banner = if reply.stdout.is_empty() {
            &reply.stderr
        } else {
            &reply.stdout
        };
        String::from_utf8_lossy(banner)
            .lines()
            .next()
            .filter(|_| reply.code == Some(0))
            .and_then(|line| {
                line.split_whitespace().find(|word| {
                    let token = word.trim_start_matches('v');
                    token.starts_with(|c: char| c.is_ascii_digit())
                        && token.contains('.')
                        && token
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b".+-_".contains(&c))
                })
            })
            .map(str::to_owned)
    } else {
        None
    };
    let mut prefix = Vec::new();
    for component in &target[1..] {
        if !parser::token(component) {
            return Err(data("invalid subcommand"));
        }
        if !verified {
            active_node(&[])?;
            verified = true;
        }
        prefix.push(component.clone());
        root = active_node(&prefix)?;
    }
    let mut tree = SkillTree::new();
    let mut queue = VecDeque::from([(Vec::new(), root)]);
    while let Some((path, node)) = queue.pop_front() {
        if path.len() < depth && !node.children.is_empty() {
            if !verified {
                active_node(&[])?;
                verified = true;
            }
            for child in node.children.keys() {
                let mut sub = path.clone();
                sub.push(child.clone());
                let args = prefix.iter().chain(&sub).cloned().collect::<Vec<_>>();
                let child_node = active_node(&args)?;
                queue.push_back((sub, child_node));
            }
        }
        tree.insert(path, node);
    }
    Ok((tree, version))
}
