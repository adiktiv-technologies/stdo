use std::{
    os::unix::process::ExitStatusExt,
    process::ExitStatus,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub struct ProcessLedger(Arc<Mutex<Vec<Process>>>);

#[derive(Debug)]
enum State {
    Exited(i32),
    Killed,
    StillRunningCheckManually,
}

impl State {
    fn label(&self) -> String {
        match self {
            Self::Exited(code) => format!("Exited({code})"),
            Self::Killed => "Killed".into(),
            Self::StillRunningCheckManually => "StillRunningCheckManually".into(),
        }
    }
}

struct Process {
    pid: u32,
    step: String,
    command: Vec<String>,
    state: State,
}

impl ProcessLedger {
    pub(super) fn record(&self, pid: u32, step: &str, command: Vec<String>) -> usize {
        let mut processes = self.0.lock().expect("process ledger lock");
        let index = processes.len();
        processes.push(Process {
            pid,
            step: step.to_owned(),
            command,
            state: State::StillRunningCheckManually,
        });
        index
    }

    pub(super) fn finished(&self, index: usize, status: ExitStatus, kill_requested: bool) {
        let state = if kill_requested && status.signal() == Some(9) {
            State::Killed
        } else {
            State::Exited(
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
            )
        };
        self.0.lock().expect("process ledger lock")[index].state = state;
    }

    pub fn print_report(&self) {
        eprintln!("Spawned Process Report (PID Ledger)");
        let processes = self.0.lock().expect("process ledger lock");
        if processes.is_empty() {
            eprintln!("  No processes spawned.");
        }
        for process in processes.iter() {
            eprintln!(
                "  PID {} | step {:?} | command {:?} | {}",
                process.pid,
                process.step,
                process.command,
                process.state.label()
            );
        }
    }
}
