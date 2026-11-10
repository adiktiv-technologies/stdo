#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;
use stdo::{
    bench,
    cli::{Cli, Command},
    dag::schema::Workflow,
    doctor,
    engine::{self, ProcessLedger, Telemetry},
    error::Error,
    report, studio,
};

#[tokio::main]
async fn main() -> ExitCode {
    let command = Cli::parse().command;
    let is_run = matches!(command, Command::Run { .. });
    let show_telemetry = matches!(
        command,
        Command::Run {
            telemetry: true,
            ..
        }
    );
    let telemetry = match &command {
        Command::Run {
            report: Some(_), ..
        } => Some(Telemetry::for_audit()),
        _ => show_telemetry.then(Telemetry::default),
    };
    let ledger = ProcessLedger::default();
    let result = dispatch(command, &ledger, telemetry.as_ref()).await;
    if let Some(report) = telemetry.filter(|_| show_telemetry) {
        report.print_report(result.is_ok());
    }
    match result {
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

async fn dispatch(
    command: Command,
    ledger: &ProcessLedger,
    telemetry: Option<&Telemetry>,
) -> Result<(), Error> {
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
        Command::Run {
            file,
            input,
            force,
            no_cache,
            report: destination,
            ..
        } => {
            let options = engine::RunOptions {
                force,
                no_cache,
                telemetry,
            };
            let mut stdout = std::io::stdout();
            if let Some(destination) = destination {
                report::run(
                    file.path(),
                    &destination,
                    &input,
                    ledger,
                    &mut stdout,
                    options,
                )
                .await?;
            } else {
                let workflow = Workflow::read(file.path())?;
                engine::run_with_options(&workflow, &input, ledger, &mut stdout, options).await?;
            }
        }
        Command::Bench {
            files,
            runs,
            warmup,
            input,
            no_cache,
            report,
        } => bench::run(&files, runs, warmup, &input, no_cache, report.as_deref()).await?,
        Command::Serve { file, port } => studio::serve(port, file.specified()).await?,
        _ => return Err(Error::InvalidWorkflow("unsupported command".into())),
    }
    Ok(())
}
