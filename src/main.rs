use std::process::ExitCode;

use clap::Parser;
use stdo::{
    cli::{Cli, Command},
    dag::schema::Workflow,
    doctor,
    error::Error,
};

const WIP_NOTICE: &str = "[WIP - Phase 1 DAG engine in progress]";

fn main() -> ExitCode {
    match dispatch(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("stdo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(command: Command) -> Result<(), Error> {
    match command {
        Command::Doctor { file } => {
            let workflow = Workflow::read(&file)?;
            let graph = doctor::check(&workflow)?;
            println!(
                "{}: {} steps in {} layers, required executables available",
                workflow.name,
                workflow.steps.len(),
                graph.layers.len()
            );
        }
        Command::Run { .. } | Command::Serve { .. } => eprintln!("{WIP_NOTICE}"),
    }
    Ok(())
}
