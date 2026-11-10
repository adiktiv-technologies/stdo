#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;
use stdo::{
    cli::{Cli, Command},
    dag::schema::Workflow,
    doctor,
    engine::{self, ProcessLedger},
    error::Error,
};

const WIP_NOTICE: &str = "[WIP - Phase 1 DAG engine in progress]";

#[tokio::main]
async fn main() -> ExitCode {
    let command = Cli::parse().command;
    let is_run = matches!(command, Command::Run { .. });
    let ledger = ProcessLedger::default();
    match dispatch(command, &ledger).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("stdo: {error}");
            if is_run {
                ledger.print_report();
            }
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(command: Command, ledger: &ProcessLedger) -> Result<(), Error> {
    match command {
        Command::Doctor { file, force } => {
            let workflow = Workflow::read(file.path())?;
            let graph = doctor::check_with_force(&workflow, force)?;
            println!(
                "{}: {} steps in {} layers, required executables available",
                workflow.name,
                workflow.steps.len(),
                graph.layers.len()
            );
        }
        Command::Run { file, input, force } => {
            let workflow = Workflow::read(file.path())?;
            engine::run(&workflow, &input, force, ledger, &mut std::io::stdout()).await?;
        }
        Command::Serve { .. } => eprintln!("{WIP_NOTICE}"),
    }
    Ok(())
}
