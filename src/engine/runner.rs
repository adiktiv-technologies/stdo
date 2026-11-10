use std::{
    io::{self, Write},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::watch,
};

use crate::error::Error;

use super::{
    budget::{Budget, Buffer},
    ledger::ProcessLedger,
    telemetry::{Status, StepTimer},
};

pub use super::budget::{MAX_STEP_OUTPUT_BYTES, MAX_WORKFLOW_MEMORY_BYTES};

const CHUNK_SIZE: usize = 64 * 1024;
const FAILURE_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) struct Captured {
    pub(super) stdout: Arc<Buffer>,
    pub(super) stderr: Buffer,
    pub(super) status: ExitStatus,
}

pub(super) struct Running {
    child: Child,
    step: String,
    input: Option<Arc<Buffer>>,
    ignore_exit_code: bool,
    ledger: ProcessLedger,
    entry: usize,
}

impl Running {
    pub(super) fn spawn(
        step: &str,
        command: Vec<String>,
        input: Option<Arc<Buffer>>,
        ignore_exit_code: bool,
        ledger: ProcessLedger,
    ) -> Result<Self, Error> {
        let child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| process_error(step, source))?;
        let entry = ledger.record(child.id().expect("new child has a PID"), step, command);
        Ok(Self {
            child,
            step: step.to_owned(),
            input,
            ignore_exit_code,
            ledger,
            entry,
        })
    }

    pub(super) async fn finish(
        mut self,
        budget: Budget,
        mut cancel: watch::Receiver<bool>,
        mut timing: Option<StepTimer>,
    ) -> Result<Option<Captured>, Error> {
        let mut stdout = self.child.stdout.take().expect("stdout is piped");
        let mut stderr = self.child.stderr.take().expect("stderr is piped");
        let mut stdin = self.child.stdin.take().expect("stdin is piped");
        let step = &self.step;
        let mut stdout_buffer = budget.buffer();
        let mut stderr_buffer = budget.buffer();
        let mut observed_status = None;
        let write_input = async move {
            if let Some(input) = self.input {
                match stdin.write_all(input.bytes()).await {
                    Err(error) if error.kind() != io::ErrorKind::BrokenPipe => {
                        return Err(process_error(step, error));
                    }
                    _ => {}
                }
            }
            Ok(())
        };
        let result = tokio::select! {
            biased;
            _ = cancel.wait_for(|cancelled| *cancelled) => Ok(None),
            result = async {
                let drain = async {
                    tokio::try_join!(
                        read_pipe(&mut stdout, &budget, step, &mut stdout_buffer),
                        read_pipe(&mut stderr, &budget, step, &mut stderr_buffer),
                        write_input,
                    )?;
                    Ok::<_, Error>(())
                };
                let wait = async {
                    let status = self.child.wait().await.map_err(|source| process_error(step, source))?;
                    self.ledger.finished(self.entry, status, false);
                    observed_status = Some(status);
                    Ok::<_, Error>(status)
                };
                tokio::pin!(drain, wait);
                let status = tokio::select! {
                    status = &mut wait => {
                        let status = status?;
                        if !status.success() && !self.ignore_exit_code {
                            // Descendants can inherit the pipes. Bound the diagnostic drain
                            // so a failed command still cancels parallel peers promptly.
                            if let Ok(result) = tokio::time::timeout(FAILURE_DRAIN_TIMEOUT, &mut drain).await {
                                result?;
                            }
                        } else {
                            drain.await?;
                        }
                        status
                    }
                    drained = &mut drain => {
                        drained?;
                        wait.await?
                    }
                };
                Ok::<_, Error>(Some(status))
            } => result,
        };
        if !matches!(result, Ok(Some(_))) && observed_status.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => self.ledger.finished(self.entry, status, false),
                _ => {
                    let requested = self.child.start_kill().is_ok();
                    if let Ok(status) = self.child.wait().await {
                        self.ledger.finished(self.entry, status, requested);
                    }
                }
            }
        }
        if let Some(timer) = &mut timing {
            timer.set_bytes(stdout_buffer.bytes().len(), stderr_buffer.bytes().len());
            if matches!(result, Ok(None)) {
                timer.set_status(Status::Cancelled);
            } else if let Some(status) = observed_status {
                timer.set_status(Status::Exit(status));
            }
        }

        let Some(status) = result? else {
            return Ok(None);
        };
        if !status.success() && !self.ignore_exit_code {
            return Err(Error::StepFailed {
                step: step.to_owned(),
                status,
                stderr: String::from_utf8_lossy(stderr_buffer.bytes())
                    .trim()
                    .to_owned(),
            });
        }
        Ok(Some(Captured {
            stdout: Arc::new(stdout_buffer),
            stderr: stderr_buffer,
            status,
        }))
    }
}

async fn read_pipe(
    mut reader: impl AsyncRead + Unpin,
    budget: &Budget,
    step: &str,
    output: &mut Buffer,
) -> Result<(), Error> {
    let mut chunk = budget.buffer();
    chunk.write_all(&[0; CHUNK_SIZE]).map_err(|source| {
        chunk
            .limit_error(step)
            .unwrap_or_else(|| process_error(step, source))
    })?;
    loop {
        let count = reader
            .read(chunk.bytes_mut())
            .await
            .map_err(|source| process_error(step, source))?;
        if count == 0 {
            return Ok(());
        }
        output
            .write_all(&chunk.bytes()[..count])
            .map_err(|source| {
                output
                    .limit_error(step)
                    .unwrap_or_else(|| process_error(step, source))
            })?;
    }
}

fn process_error(step: &str, source: io::Error) -> Error {
    Error::Process {
        step: step.to_owned(),
        source,
    }
}
