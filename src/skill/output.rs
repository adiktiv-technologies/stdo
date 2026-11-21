use crate::skill::{
    cli::{home, Cli, Config},
    exit::{data, usage, Code, Error, Result},
};
use similar::TextDiff;
use std::{
    env, fs,
    io::{self, BufRead, IsTerminal, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};
pub fn destination(args: &Cli, cfg: &Config, name: &str) -> Result<PathBuf> {
    if args.subdir.is_some() && !args.repo && args.repo_dir.is_none() {
        return Err(usage("--subdir requires repository scope"));
    }
    if let Some(p) = &args.output {
        return Ok(p.clone());
    }
    let root = if args.repo || args.repo_dir.is_some() {
        let o = Command::new("git")
            .arg("-C")
            .arg(args.repo_dir.as_deref().unwrap_or(Path::new(".")))
            .args(["rev-parse", "--show-toplevel"])
            .output()?;
        if !o.status.success() {
            return Err(data("not a Git worktree"));
        }
        let sub = cfg.subdir.as_deref().unwrap_or(Path::new(".agents/skills"));
        if sub.as_os_str().is_empty()
            || sub
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(usage(
                "subdir must be a relative directory inside the repository",
            ));
        }
        Path::new(
            std::str::from_utf8(&o.stdout)
                .map_err(|_| data("non-UTF-8 Git root"))?
                .trim_end_matches('\n'),
        )
        .join(sub)
    } else if let Some(dir) = &cfg.global_dir {
        dir.clone()
    } else if let Some(dir) = env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        PathBuf::from(dir).join("skills")
    } else {
        home()?.join(".local/share/skills")
    };
    Ok(root.join(name).join("SKILL.md"))
}
fn managed(bytes: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut lines = s.lines();
    if lines.next() != Some("---") {
        return false;
    }
    let mut header = String::new();
    for l in lines {
        if l == "---" {
            return serde_yaml::from_str::<serde_yaml::Mapping>(&header)
                .ok()
                .and_then(|m| m.get("x-generated-by").cloned())
                .is_some_and(|v| v.as_str() == Some("skill"));
        }
        header.push_str(l);
        header.push('\n');
    }
    false
}
fn stdout(s: &str) -> Result<()> {
    io::stdout().lock().write_all(s.as_bytes())?;
    Ok(())
}
pub fn write(path: &Path, content: &str, args: &Cli, no_color: bool) -> Result<Code> {
    let preview = |s: &str| -> Result<()> {
        if io::stdout().is_terminal() && !no_color {
            stdout(&format!("\x1b[36m{s}\x1b[0m"))
        } else {
            stdout(s)
        }
    };
    if path == Path::new("-") {
        if args.dry_run {
            preview(content)?;
            return Ok(Code::Diff);
        }
        stdout(content)?;
        return Ok(Code::Ok);
    }
    let old = match fs::read(path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if args.dry_run {
        if old.as_deref() == Some(content.as_bytes()) {
            return Ok(Code::Ok);
        }
        if let Some(old) = old {
            let old = String::from_utf8_lossy(&old);
            let label = path.to_string_lossy();
            let diff = TextDiff::from_lines(old.as_ref(), content)
                .unified_diff()
                .header(&label, &label)
                .to_string();
            preview(&diff)?;
        } else {
            preview(content)?
        }
        return Ok(Code::Diff);
    }
    if let Some(bytes) = &old {
        if !args.force && !managed(bytes) {
            let conflict = || Error(Code::Create, format!("unmanaged file: {}", path.display()));
            if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
                return Err(conflict());
            }
            eprint!("Replace {}? [y/N] ", path.display());
            io::stderr().flush()?;
            let mut answer = String::new();
            io::BufReader::new(fs::File::open("/dev/tty")?).read_line(&mut answer)?;
            if !matches!(answer.trim(), "y" | "Y" | "yes") {
                return Err(conflict());
            }
        }
        if bytes == content.as_bytes() {
            return Ok(Code::Ok);
        }
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content.as_bytes())?;
    if old.is_none() && !args.force {
        temp.persist_noclobber(path).map_err(|e| {
            if e.error.kind() == io::ErrorKind::AlreadyExists {
                Error(Code::Create, "target appeared during generation".into())
            } else {
                e.error.into()
            }
        })?;
    } else {
        temp.persist(path).map_err(|e| e.error)?;
    }
    Ok(Code::Ok)
}
