//! Continuous terminal supervisor. Workflow output keeps its normal stdout route.
use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    dag::{graph::Graph, schema::Workflow},
    engine::{self, ProcessLedger, RunOptions, Telemetry},
    error::Error,
};
use tokio::{
    signal::unix::{signal, SignalKind},
    sync::{oneshot, watch},
};

#[derive(Clone, Default)]
struct Stats {
    pid: Option<u32>,
    started: Option<Instant>,
    attempts: u32,
    input: usize,
    output: usize,
    ttfb: Option<Duration>,
}

#[derive(Default)]
pub(crate) struct LiveStep(Mutex<Stats>);

impl LiveStep {
    pub(crate) fn spawned(&self, pid: u32, attempts: u32) {
        let mut stats = self.0.lock().unwrap();
        stats.pid = Some(pid);
        stats.started.get_or_insert_with(Instant::now);
        stats.ttfb = None;
        stats.attempts = attempts;
    }
    pub(crate) fn input(&self, count: usize) {
        self.0.lock().unwrap().input += count;
    }
    pub(crate) fn output(&self, count: usize, started: Instant) {
        let mut stats = self.0.lock().unwrap();
        stats.output += count;
        stats.ttfb.get_or_insert_with(|| started.elapsed());
    }
}

#[derive(Clone)]
pub(crate) struct Monitor {
    pub(crate) cancel: watch::Sender<bool>,
    steps: Arc<Mutex<BTreeMap<String, Arc<LiveStep>>>>,
}

impl Monitor {
    pub(crate) fn step(&self, id: &str) -> Arc<LiveStep> {
        self.steps
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .clone()
    }
}

struct Terminal(Option<String>);

impl Terminal {
    fn open() -> io::Result<Self> {
        if !io::stdin().is_terminal() {
            return Ok(Self(None));
        }
        let saved = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()?;
        if !saved.status.success() {
            return Err(io::Error::other("could not read terminal settings"));
        }
        let terminal = Self(Some(
            String::from_utf8_lossy(&saved.stdout).trim().to_owned(),
        ));
        if !Command::new("stty")
            .args(["-icanon", "-echo", "min", "1", "time", "0"])
            .status()?
            .success()
        {
            return Err(io::Error::other("could not enable single-key input"));
        }
        Ok(terminal)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(saved) = &self.0 {
            let _ = Command::new("stty").arg(saved).status();
        }
    }
}

struct Dashboard<'a> {
    workflow: &'a Workflow,
    graph: Graph,
    interactive: bool,
}
impl Dashboard<'_> {
    fn draw(
        &self,
        telemetry: &Telemetry,
        monitor: &Monitor,
        pass: u64,
        state: &str,
    ) -> io::Result<()> {
        let report = telemetry.snapshot();
        let finished: BTreeMap<_, _> = report.steps.iter().map(|s| (s.id.as_str(), s)).collect();
        let width = self
            .workflow
            .steps
            .iter()
            .map(|s| s.id.escape_default().count())
            .max()
            .unwrap_or(0)
            .max(7);
        let mut out = io::stderr().lock();
        if self.interactive {
            write!(out, "\x1b[H\x1b[2J")?;
        }
        writeln!(
            out,
            "Layer {:width$} {:10} {:>7} {:>9} {:>12} {:>10} {:>10} Attempts/Cache",
            "Step ID", "Status", "PID", "TTFB(ms)", "Duration(ms)", "Bytes in", "Bytes out"
        )?;
        for (layer, indices) in self.graph.layers.iter().enumerate() {
            for &index in indices {
                let id = &self.workflow.steps[index].id;
                let stats = monitor.step(id).0.lock().unwrap().clone();
                let done = finished.get(id.as_str()).copied();
                let (label, color) = match done.map(|s| s.kind) {
                    Some("timed_out") => ("● TIMEOUT", 31),
                    Some("cancelled") => ("CANCEL", 33),
                    Some("skipped") => ("SKIP", 36),
                    Some("cache_hit") => ("● CACHE", 32),
                    Some("switch") => ("● PASS", 32),
                    Some("exit") if done.and_then(|s| s.exit_code) == Some(0) => ("● PASS", 32),
                    Some(_) => ("● FAIL", 31),
                    None if stats.pid.is_some() => ("◐ RUN", 33),
                    None => ("WAIT", 36),
                };
                let duration = done
                    .map(|s| s.duration_ms)
                    .or_else(|| stats.started.map(|t| t.elapsed().as_secs_f64() * 1000.0));
                let ttfb = done
                    .and_then(|s| s.ttfb_ms)
                    .or_else(|| stats.ttfb.map(|d| d.as_secs_f64() * 1000.0));
                let output = done
                    .filter(|s| s.cache_hit)
                    .map_or(stats.output, |s| s.stdout_bytes);
                let time = |v: Option<f64>| v.map_or_else(|| "-".into(), |ms| format!("{ms:.1}"));
                write!(
                    out,
                    "{:>5} {:width$} ",
                    layer + 1,
                    id.escape_default().to_string()
                )?;
                if self.interactive {
                    write!(out, "\x1b[{color}m")?;
                }
                write!(out, "{label:10}")?;
                if self.interactive {
                    write!(out, "\x1b[0m")?;
                }
                writeln!(
                    out,
                    " {:>7} {:>9} {:>12} {:>10} {:>10} {}",
                    stats.pid.map_or_else(|| "-".into(), |p| p.to_string()),
                    time(ttfb),
                    time(duration),
                    stats.input,
                    output,
                    if done.is_some_and(|s| s.cache_hit) {
                        "cache".into()
                    } else {
                        stats.attempts.to_string()
                    }
                )?;
            }
        }
        writeln!(
            out,
            "DAG {state} | Pass {pass} | Peak Monitored Buffer {} B / 32 MiB | q / Ctrl+C to quit",
            report.peak_bytes
        )?;
        out.flush()
    }
}

pub async fn run(
    path: &Path,
    inputs: &[String],
    force: bool,
    no_cache: bool,
    interval: u64,
) -> Result<(), Error> {
    let workflow = Workflow::read(path)?;
    let dashboard = Dashboard {
        graph: Graph::build_with_force(&workflow, force)?,
        workflow: &workflow,
        interactive: io::stderr().is_terminal(),
    };
    let mut interrupt = signal(SignalKind::interrupt()).map_err(Error::Terminal)?;
    let _terminal = Terminal::open().map_err(Error::Terminal)?;
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut byte = [0];
        while let Ok(1) = stdin.read(&mut byte) {
            if byte[0] == b'q' {
                let _ = tx.send(());
                break;
            }
        }
    });
    let quit = async {
        let key = async {
            if rx.await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! { _ = key => {}, _ = interrupt.recv() => {} }
    };
    tokio::pin!(quit);
    for pass in 1u64.. {
        let monitor = Monitor {
            cancel: watch::channel(false).0,
            steps: Arc::default(),
        };
        let mut telemetry = Telemetry::default();
        telemetry.monitor = Some(monitor.clone());
        let ledger = ProcessLedger::default();
        let mut stdout = io::stdout();
        let run = engine::run_with_source(
            &workflow,
            Some(path),
            inputs,
            &ledger,
            &mut stdout,
            RunOptions {
                force,
                no_cache,
                telemetry: Some(&telemetry),
            },
        );
        tokio::pin!(run);
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        let result = loop {
            tokio::select! {
                biased;
                _ = &mut quit => {
                    monitor.cancel.send_replace(true);
                    let _ = run.await;
                    return Ok(());
                }
                result = &mut run => break result,
                _ = tick.tick() => {
                    if let Err(error) = dashboard.draw(&telemetry,&monitor,pass,"RUN") {
                        monitor.cancel.send_replace(true);
                        let _ = run.await;
                        return Err(Error::Terminal(error));
                    }
                }
            }
        };
        io::stdout().flush().map_err(Error::WriteOutput)?;
        dashboard
            .draw(
                &telemetry,
                &monitor,
                pass,
                if result.is_ok() { "PASS" } else { "FAIL" },
            )
            .map_err(Error::Terminal)?;
        if let Err(error) = result {
            eprintln!("stdo: {error}");
        }
        tokio::select! { _ = &mut quit => return Ok(()), _ = tokio::time::sleep(Duration::from_millis(interval)) => {} }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_keep_total_bytes_and_refresh_first_byte_latency() {
        let step = LiveStep::default();
        step.spawned(123, 1);
        step.input(4);
        step.output(6, Instant::now());
        step.spawned(456, 2);
        let stats = step.0.lock().unwrap();
        assert_eq!((stats.pid, stats.attempts), (Some(456), 2));
        assert_eq!((stats.input, stats.output), (4, 6));
        assert!(stats.ttfb.is_none());
    }
}
