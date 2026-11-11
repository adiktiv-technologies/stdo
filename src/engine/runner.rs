use std::{
    io,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::watch,
};

use crate::error::Error;

use super::{
    budget::{Budget, Buffer},
    jsonl::Collector,
    ledger::ProcessLedger,
    pipe::{Input, Outputs, CHUNK_SIZE},
    telemetry::{Status, StepTimer},
};

pub use super::budget::{MAX_STEP_OUTPUT_BYTES, MAX_WORKFLOW_MEMORY_BYTES};

const FAILURE_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) struct Captured {
    pub(super) stdout: Arc<Buffer>,
    pub(super) stderr: Buffer,
    pub(super) status: ExitStatus,
}

#[derive(Clone)]
pub(super) struct SpawnContext {
    pub(super) call_stack: Arc<str>,
    pub(super) ledger: ProcessLedger,
}

pub(super) struct Running {
    started: Instant,
    child: Child,
    group: u32,
    step: String,
    input: Option<Input>,
    ignore_exit_code: bool,
    timeout_ms: Option<u64>,
    stream_stdout: bool,
    ledger: ProcessLedger,
    entry: usize,
}

impl Running {
    pub(super) fn spawn(
        step: &str,
        command: &[String],
        input: Option<Input>,
        ignore_exit_code: bool,
        timeout_ms: Option<u64>,
        stream_stdout: bool,
        context: &SpawnContext,
    ) -> Result<Self, Error> {
        let started = Instant::now();
        let mut process = Command::new(&command[0]);
        #[cfg(unix)]
        process.process_group(0);
        let child = process
            .args(&command[1..])
            .env(
                crate::doctor::subworkflows::CALL_STACK_ENV,
                context.call_stack.as_ref(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| process_error(step, source))?;
        let group = child.id().expect("new child has a PID");
        let entry = context.ledger.record(group, step, command.to_vec());
        Ok(Self {
            started,
            child,
            group,
            step: step.to_owned(),
            input,
            ignore_exit_code,
            timeout_ms,
            stream_stdout,
            ledger: context.ledger.clone(),
            entry,
        })
    }

    pub(super) async fn finish(
        mut self,
        budget: Budget,
        mut cancel: watch::Receiver<bool>,
        mut timing: Option<&mut StepTimer>,
        parse_jsonl: bool,
        capture_stdout: bool,
        pipes: &mut Outputs,
    ) -> Result<Option<Captured>, Error> {
        let mut stdout = self.child.stdout.take().expect("stdout is piped");
        let mut stderr = self.child.stderr.take().expect("stderr is piped");
        let stdin = self.child.stdin.take().expect("stdin is piped");
        let step = &self.step;
        let mut stdout_buffer = budget.buffer();
        let mut stderr_buffer = budget.buffer();
        let mut observed_status = None;
        let mut first_byte = None;
        let mut streamed_bytes = 0;
        let mut streamed_digest = (!capture_stdout).then(Sha256::new);
        let timeout = async {
            match self.timeout_ms {
                Some(ms) => {
                    let remaining =
                        Duration::from_millis(ms).saturating_sub(self.started.elapsed());
                    if !remaining.is_zero() {
                        tokio::time::sleep(remaining).await;
                    }
                    ms
                }
                None => std::future::pending().await,
            }
        };
        let write_input = async move {
            if let Some(input) = self.input {
                input
                    .write(stdin)
                    .await
                    .map_err(|error| process_error(step, error))?;
            } else {
                drop(stdin);
            }
            Ok(())
        };
        let result = tokio::select! {
            biased;
            _ = cancel.wait_for(|cancelled| *cancelled) => Ok(None),
            timeout_ms = timeout => Err(Error::StepTimeout { step: step.to_owned(), timeout_ms }),
            result = async {
                let drain = async {
                    let stdout_read = async {
                        let mut jsonl = parse_jsonl.then(|| Collector::new(&budget, step, capture_stdout));
                        let mut chunk = read_buffer(&budget, step)?;
                        let mut normalized = budget.buffer();
                        loop {
                            let bytes = read_chunk(&mut stdout, &mut chunk, step).await?;
                            if bytes.is_empty() { break; }
                            if let Some(timer) = timing.as_deref() {
                                first_byte.get_or_insert_with(Instant::now);
                                if self.stream_stdout {
                                    timer.stdout_chunk(bytes);
                                }
                            }
                            let bytes = if let Some(collector) = &mut jsonl {
                                collector.push(bytes, &mut normalized)?;
                                normalized.bytes()
                            } else {
                                bytes
                            };
                            if capture_stdout {
                                stdout_buffer.append(bytes, step)?;
                            } else if let Some(digest) = &mut streamed_digest {
                                streamed_bytes += bytes.len();
                                digest.update(bytes);
                            }
                            pipes.forward(bytes, &budget, step).await?;
                            normalized.clear();
                        }
                        if let Some(collector) = jsonl {
                            collector.finish(&mut normalized)?;
                            if capture_stdout {
                                stdout_buffer.append(normalized.bytes(), step)?;
                            } else if let Some(digest) = &mut streamed_digest {
                                streamed_bytes += normalized.bytes().len();
                                digest.update(normalized.bytes());
                            }
                            pipes.forward(normalized.bytes(), &budget, step).await?;
                        }
                        Ok::<_, Error>(())
                    };
                    tokio::try_join!(
                        stdout_read,
                        read_pipe(&mut stderr, &budget, step, |bytes| stderr_buffer.append(bytes, step)),
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
        let failed = !matches!(result, Ok(Some(status)) if status.success());
        let group_signalled = if failed {
            kill_group(self.group).await
        } else {
            false
        };
        if failed && observed_status.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    observed_status = Some(status);
                    self.ledger.finished(self.entry, status, group_signalled);
                }
                _ => {
                    let requested = self.child.start_kill().is_ok();
                    if let Ok(status) = self.child.wait().await {
                        observed_status = Some(status);
                        self.ledger
                            .finished(self.entry, status, requested || group_signalled);
                    }
                }
            }
        }
        if matches!(result, Err(Error::StepTimeout { .. })) && observed_status.is_some() {
            self.ledger.timed_out(self.entry);
        }
        if let Some(timer) = &mut timing {
            if let Some(at) = first_byte {
                timer.first_byte(self.started, at);
            }
            if let Some(digest) = streamed_digest {
                timer.set_stream_stats(
                    streamed_bytes,
                    Some(format!("{:x}", digest.finalize())),
                    stderr_buffer.bytes(),
                );
            } else {
                timer.set_streams(stdout_buffer.bytes(), stderr_buffer.bytes());
            }
            if matches!(result, Ok(None)) {
                timer.set_status(Status::Cancelled);
            } else if matches!(result, Err(Error::StepTimeout { .. })) {
                timer.set_status(Status::TimedOut);
            } else if let Ok(Some(status)) = result {
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

    pub(super) fn group(&self) -> u32 {
        self.group
    }
}

// Signal the Unix group through the system utility.
pub(super) async fn kill_group(group: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

async fn read_pipe(
    mut reader: impl AsyncRead + Unpin,
    budget: &Budget,
    step: &str,
    mut consume: impl FnMut(&[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut chunk = read_buffer(budget, step)?;
    loop {
        let bytes = read_chunk(&mut reader, &mut chunk, step).await?;
        if bytes.is_empty() {
            return Ok(());
        }
        consume(bytes)?;
    }
}

fn read_buffer(budget: &Budget, step: &str) -> Result<Buffer, Error> {
    let mut chunk = budget.buffer();
    chunk
        .resize_zeroed(CHUNK_SIZE)
        .map_err(|source| chunk.stream_error(step, source))?;
    Ok(chunk)
}

async fn read_chunk<'a>(
    reader: &mut (impl AsyncRead + Unpin),
    chunk: &'a mut Buffer,
    step: &str,
) -> Result<&'a [u8], Error> {
    let count = reader
        .read(chunk.bytes_mut())
        .await
        .map_err(|source| process_error(step, source))?;
    Ok(&chunk.bytes()[..count])
}

fn process_error(step: &str, source: io::Error) -> Error {
    Error::Process {
        step: step.to_owned(),
        source,
    }
}
