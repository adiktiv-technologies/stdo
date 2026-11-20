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
        self.specified().unwrap_or_else(|| Path::new("stdo.yaml"))
    }

    pub fn specified(&self) -> Option<&Path> {
        self.named_file
            .as_deref()
            .or(self.positional_file.as_deref())
    }
}

#[derive(Subcommand)]
#[non_exhaustive]
pub enum Command {
    /// Execute a workflow DAG.
    Run {
        #[command(flatten)]
        file: WorkflowFile,
        /// Override a workflow input (repeatable).
        #[arg(short, long, value_name = "KEY=VALUE")]
        input: Vec<String>,
        /// Report workflow timings and monitored buffer usage on stderr.
        #[arg(short = 't', long)]
        telemetry: bool,
        /// Bypass reading and writing the step cache.
        #[arg(long)]
        no_cache: bool,
        /// Write an offline JSON or HTML execution audit.
        #[arg(long, value_name = "PATH")]
        report: Option<PathBuf>,
        /// Bypass graph size limits.
        #[arg(long)]
        force: bool,
    },
    /// Monitor one DAG pass on stderr, then hold the result until r or q.
    Top {
        #[command(flatten)]
        file: WorkflowFile,
        /// Override a workflow input (repeatable).
        #[arg(short, long, value_name = "KEY=VALUE")]
        input: Vec<String>,
        /// Bypass reading and writing the step cache.
        #[arg(long)]
        no_cache: bool,
        /// Bypass graph size limits.
        #[arg(long)]
        force: bool,
        /// Automatically start another pass after each result.
        #[arg(long, conflicts_with = "once")]
        watch: bool,
        /// Delay in milliseconds between passes in watch mode.
        #[arg(long, value_name = "MS", default_value_t = 1000)]
        interval: u64,
        /// Monitor one pass and exit with its result.
        #[arg(long, conflicts_with = "interval")]
        once: bool,
    },
    /// Compare the performance of one or more workflows.
    Bench {
        /// Workflow files in comparison order (baseline first).
        #[arg(required = true, num_args = 1.., value_name = "FILES")]
        files: Vec<PathBuf>,
        /// Measured trials per workflow.
        #[arg(long, default_value_t = 5)]
        runs: usize,
        /// Warmup trials per workflow.
        #[arg(long, default_value_t = 1)]
        warmup: usize,
        /// Override an input for all workflows (repeatable).
        #[arg(short, long, value_name = "KEY=VALUE")]
        input: Vec<String>,
        /// Bypass reading and writing the step cache.
        #[arg(long)]
        no_cache: bool,
        /// Write an offline JSON or HTML benchmark comparison.
        #[arg(long, value_name = "PATH")]
        report: Option<PathBuf>,
    },
    /// Launch the offline Studio UI on 127.0.0.1.
    Serve {
        #[command(flatten)]
        file: WorkflowFile,
        /// TCP port for the loopback listener.
        #[arg(long, default_value_t = 8080)]
        port: u16,
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
