mod cli;
mod exit;
mod filter;
mod init;
mod introspect;
mod output;
mod render;
mod stdo;
pub use cli::Cli as SkillArgs;
use exit::{Code, Result};
pub use init::argv;
use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};
fn run(args: cli::Cli) -> Result<Code> {
    if let Some(shell) = args.init {
        io::stdout()
            .lock()
            .write_all(init::wrapper(shell).as_bytes())?;
        return Ok(Code::Ok);
    }
    let cfg = cli::Config::load(&args)?;
    let name = cli::name(&args.target, args.name.as_deref())?;
    let path = output::destination(&args, &cfg, &name)?;
    let prefix = &args.target[1..];
    let (mut tree, version) = introspect::discover(
        &args.target,
        args.max_depth,
        cfg.timeout_ms.unwrap_or(2000),
        args.verbose && !args.dry_run,
        !args.list,
    )?;
    let whitelist = filter::apply(&mut tree, prefix, &args.include, &args.exclude)?;
    if args.list {
        let mut out = io::stdout().lock();
        for key in tree.keys().filter(|key| !key.is_empty()) {
            writeln!(
                out,
                "{}",
                prefix
                    .iter()
                    .chain(key)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            )?
        }
        return Ok(Code::Ok);
    }
    let (content, trimmed) = render::render(
        &tree,
        &args.target.join(" "),
        &name,
        version.as_deref(),
        cfg.max_lines.unwrap_or(200),
        (args.include.is_empty()
            && args.exclude.is_empty()
            && !whitelist
            && stdo::is_current(&args.target[0]))
        .then_some(prefix),
    )?;
    let code = output::write(&path, &content, &args, cfg.no_color.unwrap_or(false))?;
    if !args.quiet && !(args.dry_run && matches!(code, Code::Ok)) {
        if trimmed {
            eprintln!("skill: document pruned to the line limit");
        }
        if !args.dry_run && io::stderr().is_terminal() && path != std::path::Path::new("-") {
            eprintln!("skill: {}", path.display());
        }
    }
    Ok(code)
}
pub fn execute(args: SkillArgs) -> ExitCode {
    match run(args) {
        Ok(c) => ExitCode::from(c as u8),
        Err(e) => {
            eprintln!("skill: {e}");
            ExitCode::from(e.0 as u8)
        }
    }
}
