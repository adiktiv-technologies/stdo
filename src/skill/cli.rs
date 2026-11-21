use crate::skill::exit::{data, usage, Result};
use clap::{ArgGroup, Args, ValueEnum};
use serde::Deserialize;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Args)]
#[command(about="Discover a local CLI and generate a scoped SKILL.md",group(ArgGroup::new("destination").args(["global","repo","repo_dir","output"])))]
pub struct Cli {
    /// Emit shell wrappers for executable commands on PATH.
    #[arg(long, value_enum, exclusive = true)]
    pub init: Option<Shell>,
    /// Install in the global skills directory (default).
    #[arg(short = 'g', long)]
    pub global: bool,
    /// Install at the current Git root.
    #[arg(short = 'R', long)]
    pub repo: bool,
    /// Resolve the Git root from DIR.
    #[arg(long, value_name = "DIR")]
    pub repo_dir: Option<PathBuf>,
    /// Relative skills directory within the repository.
    #[arg(long, value_name = "REL_DIR")]
    pub subdir: Option<PathBuf>,
    /// Write to FILE, or use - for stdout.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Override the generated lowercase skill name.
    #[arg(long, value_name = "SLUG")]
    pub name: Option<String>,
    /// List discovered subcommand paths, one per line.
    #[arg(short, long)]
    pub list: bool,
    /// Maximum depth below the selected command.
    #[arg(short = 'd', long, default_value_t = 2)]
    pub max_depth: usize,
    /// Include matching command paths or flags (repeatable).
    #[arg(short, long, value_name = "GLOB")]
    pub include: Vec<String>,
    /// Exclude matching command paths or flags (repeatable).
    #[arg(short = 'x', long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Maximum document lines (default 200, zero is unlimited).
    #[arg(short = 'L', long)]
    pub max_lines: Option<usize>,
    /// Introspection timeout in milliseconds (default 2000).
    #[arg(long, value_name = "MS")]
    pub timeout: Option<u64>,
    /// Preview creation or show a unified diff.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Replace an unmanaged document without prompting.
    #[arg(short, long)]
    pub force: bool,
    /// Suppress progress and warnings.
    #[arg(short, long, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Report introspection diagnostics.
    #[arg(short, long)]
    pub verbose: bool,
    /// Target executable followed by optional subcommands.
    #[arg(last=true,num_args=1..,required_unless_present="init",value_name="CLI [SUBCOMMAND...]")]
    pub target: Vec<String>,
}

#[derive(Default, Deserialize)]
pub struct Config {
    pub global_dir: Option<PathBuf>,
    pub subdir: Option<PathBuf>,
    pub max_lines: Option<usize>,
    pub timeout_ms: Option<u64>,
    pub no_color: Option<bool>,
}
pub fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| data("HOME is unset"))
}
fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}
fn number<T: std::str::FromStr>(key: &str) -> Result<Option<T>> {
    env::var(key)
        .ok()
        .map(|v| v.parse().map_err(|_| usage(format!("invalid {key}"))))
        .transpose()
}
impl Config {
    pub fn load(args: &Cli) -> Result<Self> {
        let dir = env_path("XDG_CONFIG_HOME").or_else(|| home().ok().map(|p| p.join(".config")));
        let mut cfg: Self = if let Some(dir) = dir {
            match fs::read_to_string(dir.join("skill/config.toml")) {
                Ok(s) => toml::from_str(&s).map_err(|e| data(format!("config.toml: {e}")))?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
                Err(e) => return Err(e.into()),
            }
        } else {
            Self::default()
        };
        cfg.global_dir = env_path("SKILL_GLOBAL_DIR").or(cfg.global_dir);
        cfg.subdir = args
            .subdir
            .clone()
            .or_else(|| env_path("SKILL_SUBDIR"))
            .or(cfg.subdir);
        cfg.max_lines = match args.max_lines {
            Some(n) => Some(n),
            None => number("SKILL_MAX_LINES")?.or(cfg.max_lines),
        };
        cfg.timeout_ms = match args.timeout {
            Some(n) => Some(n),
            None => number("SKILL_TIMEOUT_MS")?.or(cfg.timeout_ms),
        };
        cfg.no_color = Some(env::var_os("NO_COLOR").is_some() || cfg.no_color.unwrap_or(false));
        Ok(cfg)
    }
}
pub fn name(target: &[String], explicit: Option<&str>) -> Result<String> {
    let stem = Path::new(&target[0])
        .file_name()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    let derived = std::iter::once(stem)
        .chain(target[1..].iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase();
    let slug = explicit.map(str::to_owned).unwrap_or_else(|| {
        derived
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    });
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return Err(usage("name must match [a-z0-9-]+"));
    }
    Ok(slug)
}
