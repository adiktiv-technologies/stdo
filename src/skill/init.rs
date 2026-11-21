use crate::skill::cli::Shell;
use clap::CommandFactory;
use std::{collections::BTreeSet, env, ffi::OsString, fs, path::Path};

pub fn wrapper(shell: Shell) -> String {
    let mut names = BTreeSet::new();
    for dir in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let Ok(name) = e.file_name().into_string() else {
                    continue;
                };
                if name != "stdo"
                    && super::introspect::parser::token(&name)
                    && !name.contains('.')
                    && super::introspect::executable(&name).is_some()
                {
                    names.insert(name);
                }
            }
        }
    }
    let mut out = String::new();
    for name in names {
        let body=match shell{
            Shell::Bash=>format!("if [[ $(type -t '{name}') == file ]]; then\n  function {name} {{ if [[ ${{1-}} == skill ]]; then shift; SKILL_WRAPPED_CLI='{name}' command stdo skill \"$@\"; else command '{name}' \"$@\"; fi; }}\nfi\n"),
            Shell::Zsh=>format!("if [[ ${{+commands[{name}]}} == 1 && ${{+builtins[{name}]}} == 0 && ${{+functions[{name}]}} == 0 && ${{+aliases[{name}]}} == 0 ]]; then\n  function {name} {{ if [[ ${{1-}} == skill ]]; then shift; SKILL_WRAPPED_CLI='{name}' command stdo skill \"$@\"; else command '{name}' \"$@\"; fi; }}\nfi\n"),
            Shell::Fish=>format!("if not functions -q '{name}'; and not builtin -q '{name}'\n function {name}\n  if test \"$argv[1]\" = skill\n   set -l -x SKILL_WRAPPED_CLI '{name}'\n   command stdo skill $argv[2..-1]\n  else\n   command '{name}' $argv\n  end\n end\nend\n"),
        };
        out.push_str(&body);
    }
    out
}
// Insert the canonical delimiter at the end of the option list for shell/plugin sugar.
pub fn argv() -> Vec<OsString> {
    let mut args: Vec<_> = env::args_os().collect();
    let bin = Path::new(&args[0])
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    let plugin = bin.strip_suffix("-skill").map(OsString::from);
    if plugin.is_some() {
        args.insert(1, "skill".into());
    }
    if args.get(1).is_none_or(|arg| arg != "skill") {
        return args;
    }
    let Some(cli) = plugin.or_else(|| env::var_os("SKILL_WRAPPED_CLI")) else {
        return args;
    };
    let root = crate::cli::Cli::command();
    let spec = root
        .find_subcommand("skill")
        .expect("registered skill command");
    let mut i = 2;
    while i < args.len() {
        let s = args[i].to_string_lossy();
        if s == "--" {
            args.insert(i + 1, cli);
            return args;
        }
        if !s.starts_with('-') || s == "-" {
            break;
        }
        let takes_next = if let Some(long) = s.strip_prefix("--") {
            spec.get_arguments()
                .any(|a| a.get_long() == Some(long) && a.get_action().takes_values())
        } else {
            let mut chars = s[1..].chars().peekable();
            let mut next = false;
            while let Some(c) = chars.next() {
                if spec
                    .get_arguments()
                    .any(|a| a.get_short() == Some(c) && a.get_action().takes_values())
                {
                    next = chars.peek().is_none();
                    break;
                }
            }
            next
        };
        i += 1 + usize::from(takes_next && i + 1 < args.len());
    }
    args.splice(i..i, [OsString::from("--"), cli]);
    args
}
