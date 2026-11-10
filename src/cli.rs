use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    version,
    about = "Standard I/O Orchestrator",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args)]
pub struct WorkflowFile {
    /// Workflow file (defaults to stdo.yaml).
    #[arg(value_name = "FILE")]
    positional_file: Option<PathBuf>,
    /// Workflow file, as an alternative to the positional argument.
    #[arg(
        short = 'f',
        long = "file",
        value_name = "FILE",
        conflicts_with = "positional_file"
    )]
    named_file: Option<PathBuf>,
}

impl WorkflowFile {
    pub fn path(&self) -> &Path {
        self.named_file
            .as_deref()
            .or(self.positional_file.as_deref())
            .unwrap_or_else(|| Path::new("stdo.yaml"))
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Execute a workflow DAG.
    Run {
        #[command(flatten)]
        file: WorkflowFile,
        #[arg(short, long, value_name = "KEY=VALUE")]
        input: Vec<String>,
        /// Report workflow timings and monitored buffer usage on stderr.
        #[arg(short = 't', long)]
        telemetry: bool,
        /// Bypass reading and writing the step cache.
        #[arg(long)]
        no_cache: bool,
        /// Bypass graph size limits.
        #[arg(long)]
        force: bool,
    },
    /// Launch the offline Studio UI (in development).
    Serve {
        #[arg(default_value = "stdo.yaml")]
        file: PathBuf,
        #[arg(long)]
        watch: bool,
    },
    /// Validate the workflow DAG and check required executables.
    Doctor {
        #[command(flatten)]
        file: WorkflowFile,
        /// Bypass graph size limits.
        #[arg(long)]
        force: bool,
    },
}
