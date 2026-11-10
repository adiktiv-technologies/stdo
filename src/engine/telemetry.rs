use std::{
    collections::BTreeMap,
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::budget::{Budget, MAX_WORKFLOW_MEMORY_BYTES};

#[derive(Clone)]
pub struct Telemetry {
    started: Instant,
    pub(super) budget: Budget,
    records: Arc<Mutex<Records>>,
    capture_stderr: bool,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            budget: Budget::default(),
            records: Arc::default(),
            capture_stderr: false,
        }
    }
}

#[derive(Default)]
struct Records {
    layers: BTreeMap<usize, Duration>,
    steps: BTreeMap<(usize, usize), Step>,
}

struct Step {
    id: String,
    duration: Duration,
    status: Status,
    executed: bool,
    stdout: usize,
    stderr: usize,
    diagnostic: Vec<u8>,
}

pub(super) enum Status {
    Exit(ExitStatus),
    CacheHit(ExitStatus),
    Switch(String),
    Skipped,
    Cancelled,
    Failed,
}

impl std::fmt::Display for Status {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exit(status) => match status.code() {
                Some(code) => write!(formatter, "exit {code}"),
                None => write!(formatter, "{status}"),
            },
            Self::CacheHit(status) => write!(formatter, "cache hit ({})", Self::Exit(*status)),
            Self::Switch(target) => write!(formatter, "switch -> {target}"),
            Self::Skipped => formatter.write_str("skipped"),
            Self::Cancelled => formatter.write_str("cancelled"),
            Self::Failed => formatter.write_str("failed"),
        }
    }
}

impl Telemetry {
    pub(crate) fn for_studio() -> Self {
        Self {
            capture_stderr: true,
            ..Self::default()
        }
    }

    pub(crate) fn studio_report(&self) -> (serde_json::Value, String, bool) {
        let records = self.records.lock().expect("telemetry lock");
        let mut stderr = Vec::new();
        let mut truncated = false;
        let steps: Vec<_> = records.steps.iter().map(|((layer, _), step)| {
            stderr.extend_from_slice(&step.diagnostic);
            truncated |= step.diagnostic.len() < step.stderr;
            let (kind, exit_code) = match &step.status {
                Status::Exit(status) => ("exit", status.code()),
                Status::CacheHit(status) => ("cache_hit", status.code()),
                Status::Switch(_) => ("switch", None),
                Status::Skipped => ("skipped", None),
                Status::Cancelled => ("cancelled", None),
                Status::Failed => ("failed", None),
            };
            serde_json::json!({
                "id": step.id, "layer": layer, "status": step.status.to_string(), "kind": kind,
                "exit_code": exit_code, "cache_hit": kind == "cache_hit",
                "duration_ms": milliseconds(step.duration), "stdout_bytes": step.stdout, "stderr_bytes": step.stderr
            })
        }).collect();
        let layers: Vec<_> = records.layers.iter().map(|(layer, duration)| {
            serde_json::json!({"layer": layer, "duration_ms": milliseconds(*duration)})
        }).collect();
        let report = serde_json::json!({
            "steps": steps, "layers": layers, "duration_ms": milliseconds(self.started.elapsed()),
            "peak_bytes": self.budget.peak_bytes(), "limit_bytes": MAX_WORKFLOW_MEMORY_BYTES
        });
        (
            report,
            String::from_utf8_lossy(&stderr).into_owned(),
            truncated,
        )
    }

    pub(super) fn layer(&self, number: usize) -> LayerTimer<'_> {
        LayerTimer {
            telemetry: self,
            number,
            started: Instant::now(),
        }
    }

    pub(super) fn step(&self, layer: usize, index: usize, id: &str) -> StepTimer {
        StepTimer {
            telemetry: self.clone(),
            key: (layer, index),
            started: Instant::now(),
            record: Some(Step {
                id: id.to_owned(),
                duration: Duration::ZERO,
                status: Status::Failed,
                executed: false,
                stdout: 0,
                stderr: 0,
                diagnostic: Vec::new(),
            }),
        }
    }

    pub fn print_report(&self, succeeded: bool) {
        let total = self.started.elapsed();
        let records = self.records.lock().expect("telemetry lock");
        eprintln!(
            "Execution Telemetry ({})",
            if succeeded {
                "completed"
            } else {
                "partial; failed"
            }
        );
        for (layer, duration) in &records.layers {
            eprintln!("  Layer {layer}: {:.2} ms", milliseconds(*duration));
            for (_, step) in records.steps.range((*layer, 0)..=(*layer, usize::MAX)) {
                eprintln!(
                    "    Step {:?}: {:.2} ms | {} | stdout_bytes={} stderr_bytes={}",
                    step.id,
                    milliseconds(step.duration),
                    step.status,
                    step.stdout,
                    step.stderr
                );
            }
        }
        let executed = records.steps.values().filter(|step| step.executed).count();
        let cached = records
            .steps
            .values()
            .filter(|step| matches!(step.status, Status::CacheHit(_)))
            .count();
        let switches = records
            .steps
            .values()
            .filter(|step| matches!(step.status, Status::Switch(_)))
            .count();
        let skipped = records
            .steps
            .values()
            .filter(|step| matches!(step.status, Status::Skipped))
            .count();
        eprintln!(
            "  Total: {:.2} ms | executed={executed} switches={switches} skipped={skipped} cached={cached} | peak_bytes={} limit_bytes={MAX_WORKFLOW_MEMORY_BYTES}",
            milliseconds(total), self.budget.peak_bytes()
        );
    }
}

pub(super) struct LayerTimer<'a> {
    telemetry: &'a Telemetry,
    number: usize,
    started: Instant,
}

impl Drop for LayerTimer<'_> {
    fn drop(&mut self) {
        self.telemetry
            .records
            .lock()
            .expect("telemetry lock")
            .layers
            .insert(self.number, self.started.elapsed());
    }
}

pub(super) struct StepTimer {
    telemetry: Telemetry,
    key: (usize, usize),
    started: Instant,
    record: Option<Step>,
}

impl StepTimer {
    pub(super) fn set_status(&mut self, status: Status) {
        self.record.as_mut().expect("active step").status = status;
    }

    pub(super) fn spawned(&mut self) {
        self.record.as_mut().expect("active step").executed = true;
    }

    pub(super) fn set_streams(&mut self, stdout: &[u8], stderr: &[u8]) {
        let record = self.record.as_mut().expect("active step");
        record.stdout = stdout.len();
        record.stderr = stderr.len();
        if self.telemetry.capture_stderr {
            record
                .diagnostic
                .extend_from_slice(&stderr[..stderr.len().min(16 * 1024)]);
        }
    }
}

impl Drop for StepTimer {
    fn drop(&mut self) {
        let mut record = self.record.take().expect("active step");
        if !matches!(record.status, Status::Skipped) {
            record.duration = self.started.elapsed();
        }
        self.telemetry
            .records
            .lock()
            .expect("telemetry lock")
            .steps
            .insert(self.key, record);
    }
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
