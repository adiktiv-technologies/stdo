use std::path::PathBuf;

use clap::{Parser, Subcommand};

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

#[derive(Subcommand)]
pub enum Command {
    /// Execute a workflow DAG.
    Run {
        #[arg(default_value = "stdo.yaml")]
        file: PathBuf,
        #[arg(short, long, value_name = "KEY=VALUE")]
        input: Vec<String>,
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
        #[arg(default_value = "stdo.yaml")]
        file: PathBuf,
        /// Bypass graph size limits.
        #[arg(long)]
        force: bool,
    },
}
