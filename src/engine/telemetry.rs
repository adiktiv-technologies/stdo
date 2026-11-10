use std::{
    collections::BTreeMap,
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::budget::{Budget, MAX_WORKFLOW_MEMORY_BYTES};
use super::events::{EngineEvent, Events};
use serde::Serialize;

#[derive(Clone)]
pub struct Telemetry {
    started: Instant,
    pub(super) budget: Budget,
    records: Arc<Mutex<Records>>,
    capture_stderr: bool,
    hash_stdout: bool,
    events: Option<Events>,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            budget: Budget::default(),
            records: Arc::default(),
            capture_stderr: false,
            hash_stdout: false,
            events: None,
        }
    }
}

#[derive(Default)]
struct Records {
    finished: Option<Instant>,
    layers: BTreeMap<usize, (Instant, Instant)>,
    steps: BTreeMap<(usize, usize), Step>,
}

struct Step {
    id: String,
    duration: Duration,
    status: Status,
    executed: bool,
    attempts: u32,
    stdout: usize,
    stderr: usize,
    diagnostic: Vec<u8>,
    ttfb: Option<Duration>,
    first_byte: Option<Instant>,
    stdout_sha256: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ExecutionTelemetry {
    pub duration_ms: f64,
    pub ttfb_ms: f64,
    pub peak_bytes: usize,
    pub limit_bytes: usize,
    pub layers: Vec<LayerTelemetry>,
    pub steps: Vec<StepTelemetry>,
}

#[derive(Serialize)]
pub(crate) struct LayerTelemetry {
    pub layer: usize,
    pub duration_ms: f64,
}

#[derive(Serialize)]
pub(crate) struct StepTelemetry {
    pub id: String,
    pub layer: usize,
    pub status: String,
    pub kind: &'static str,
    pub exit_code: Option<i32>,
    pub cache_hit: bool,
    pub attempts: u32,
    pub duration_ms: f64,
    pub ttfb_ms: Option<f64>,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_sha256: Option<String>,
}

pub(super) enum Status {
    Exit(ExitStatus),
    CacheHit(ExitStatus),
    Switch(String),
    Skipped,
    Cancelled,
    Failed,
    TimedOut,
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
            Self::TimedOut => formatter.write_str("timed out"),
        }
    }
}

impl Telemetry {
    pub fn for_audit() -> Self {
        Self {
            hash_stdout: true,
            ..Self::default()
        }
    }

    pub(crate) fn with_stdout_hashes(&self) -> Self {
        Self {
            hash_stdout: true,
            ..self.clone()
        }
    }

    /// Elapsed time to the first step stdout, or elapsed run time if all steps were silent.
    pub fn first_byte_duration(&self) -> Duration {
        let records = self.records.lock().expect("telemetry lock");
        self.first_byte_duration_at(&records, self.duration(&records))
    }

    pub(crate) fn finish(&self) {
        self.records
            .lock()
            .expect("telemetry lock")
            .finished
            .get_or_insert_with(Instant::now);
    }

    fn duration(&self, records: &Records) -> Duration {
        records.finished.map_or_else(
            || self.started.elapsed(),
            |finished| finished.duration_since(self.started),
        )
    }

    pub(crate) fn for_studio() -> Self {
        Self {
            capture_stderr: true,
            ..Self::default()
        }
    }

    pub(crate) fn with_events(mut self, events: Events) -> Self {
        self.events = Some(events);
        self
    }

    pub(crate) fn studio_report(&self) -> (serde_json::Value, String, bool) {
        let records = self.records.lock().expect("telemetry lock");
        let mut stderr = Vec::new();
        let mut truncated = false;
        for step in records.steps.values() {
            stderr.extend_from_slice(&step.diagnostic);
            truncated |= step.diagnostic.len() < step.stderr;
        }
        (
            serde_json::json!(self.snapshot_records(&records)),
            String::from_utf8_lossy(&stderr).into_owned(),
            truncated,
        )
    }

    pub(crate) fn snapshot(&self) -> ExecutionTelemetry {
        self.snapshot_records(&self.records.lock().expect("telemetry lock"))
    }

    fn snapshot_records(&self, records: &Records) -> ExecutionTelemetry {
        let total = self.duration(records);
        let steps = records
            .steps
            .iter()
            .map(|((layer, _), step)| step.snapshot(*layer, self.hash_stdout))
            .collect();
        let layers = records
            .layers
            .iter()
            .map(|(layer, (started, finished))| LayerTelemetry {
                layer: *layer,
                duration_ms: milliseconds(finished.duration_since(*started)),
            })
            .collect();
        ExecutionTelemetry {
            steps,
            layers,
            duration_ms: milliseconds(total),
            ttfb_ms: milliseconds(self.first_byte_duration_at(records, total)),
            peak_bytes: self.budget.peak_bytes(),
            limit_bytes: MAX_WORKFLOW_MEMORY_BYTES,
        }
    }

    fn first_byte_duration_at(&self, records: &Records, total: Duration) -> Duration {
        records
            .steps
            .values()
            .filter_map(|step| step.first_byte)
            .min()
            .map_or(total, |at| at.duration_since(self.started))
    }

    pub(super) fn layer(&self, number: usize) -> LayerTimer<'_> {
        if let Some(events) = &self.events {
            if !self
                .records
                .lock()
                .expect("telemetry lock")
                .layers
                .contains_key(&number)
            {
                events.emit(EngineEvent::LayerStarted { layer: number });
            }
        }
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
                attempts: 0,
                stdout: 0,
                stderr: 0,
                diagnostic: Vec::new(),
                ttfb: None,
                first_byte: None,
                stdout_sha256: None,
            }),
        }
    }

    pub fn print_report(&self, succeeded: bool) {
        let records = self.records.lock().expect("telemetry lock");
        let total = self.duration(&records);
        eprintln!(
            "Execution Telemetry ({})",
            if succeeded {
                "completed"
            } else {
                "partial; failed"
            }
        );
        for (layer, (started, finished)) in &records.layers {
            eprintln!(
                "  Layer {layer}: {:.2} ms",
                milliseconds(finished.duration_since(*started))
            );
            for (_, step) in records.steps.range((*layer, 0)..=(*layer, usize::MAX)) {
                let ttfb = match step.ttfb {
                    Some(duration) => format!(" | ttfb={:.2} ms", milliseconds(duration)),
                    None if step.executed => " | ttfb=n/a".into(),
                    None => String::new(),
                };
                let attempts = if step.attempts > 1 {
                    format!(" | attempts={}", step.attempts)
                } else {
                    String::new()
                };
                eprintln!(
                    "    Step {:?}: {:.2} ms | {} | stdout_bytes={} stderr_bytes={}{ttfb}{attempts}",
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
            "  Total: {:.2} ms | executed={executed} switches={switches} skipped={skipped} cached={cached} | ttfb={:.2} ms | peak_bytes={} limit_bytes={MAX_WORKFLOW_MEMORY_BYTES}",
            milliseconds(total), milliseconds(self.first_byte_duration_at(&records, total)), self.budget.peak_bytes()
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
        let finished = Instant::now();
        self.telemetry
            .records
            .lock()
            .expect("telemetry lock")
            .layers
            .entry(self.number)
            .and_modify(|range| range.1 = finished)
            .or_insert((self.started, finished));
    }
}

pub(super) struct StepTimer {
    telemetry: Telemetry,
    key: (usize, usize),
    started: Instant,
    record: Option<Step>,
}

impl StepTimer {
    pub(super) fn started(&self) {
        if let Some(events) = &self.telemetry.events {
            let id = self.record.as_ref().expect("active step").id.clone();
            events.emit(EngineEvent::StepStarted {
                id,
                layer: self.key.0,
            });
        }
    }

    pub(super) fn stdout_chunk(&self, bytes: &[u8]) {
        if let Some(events) = &self.telemetry.events {
            events.stdout(&self.record.as_ref().expect("active step").id, bytes);
        }
    }

    pub(super) fn first_byte(&mut self, started: Instant, at: Instant) {
        let record = self.record.as_mut().expect("active step");
        record.ttfb = Some(at.duration_since(started));
        record.first_byte.get_or_insert(at);
    }

    pub(super) fn cache_read(&mut self, started: Instant, at: Instant, has_stdout: bool) {
        let record = self.record.as_mut().expect("active step");
        record.ttfb = Some(at.duration_since(started));
        record.first_byte = has_stdout.then_some(at);
    }

    pub(super) fn set_status(&mut self, status: Status) {
        self.record.as_mut().expect("active step").status = status;
    }

    pub(super) fn spawned(&mut self) {
        let record = self.record.as_mut().expect("active step");
        record.executed = true;
        record.attempts += 1;
        record.ttfb = None;
        record.status = Status::Failed;
    }

    pub(super) fn set_streams(&mut self, stdout: &[u8], stderr: &[u8]) {
        let digest = self
            .telemetry
            .hash_stdout
            .then(|| crate::report::sha256(stdout));
        self.set_stream_stats(stdout.len(), digest, stderr);
    }

    pub(super) fn set_stream_stats(
        &mut self,
        stdout: usize,
        digest: Option<String>,
        stderr: &[u8],
    ) {
        let record = self.record.as_mut().expect("active step");
        record.stdout = stdout;
        record.stderr = stderr.len();
        if self.telemetry.hash_stdout {
            record.stdout_sha256 = digest;
        }
        if self.telemetry.capture_stderr {
            record.diagnostic.clear();
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
        if let Some(events) = &self.telemetry.events {
            events.emit(EngineEvent::StepFinished {
                telemetry: record.snapshot(self.key.0, self.telemetry.hash_stdout),
            });
        }
        self.telemetry
            .records
            .lock()
            .expect("telemetry lock")
            .steps
            .insert(self.key, record);
    }
}

impl Step {
    fn snapshot(&self, layer: usize, hash_stdout: bool) -> StepTelemetry {
        let (kind, exit_code) = match &self.status {
            Status::Exit(status) => ("exit", status.code()),
            Status::CacheHit(status) => ("cache_hit", status.code()),
            Status::Switch(_) => ("switch", None),
            Status::Skipped => ("skipped", None),
            Status::Cancelled => ("cancelled", None),
            Status::Failed => ("failed", None),
            Status::TimedOut => ("timed_out", None),
        };
        StepTelemetry {
            id: self.id.clone(),
            layer,
            status: self.status.to_string(),
            kind,
            exit_code,
            cache_hit: kind == "cache_hit",
            attempts: self.attempts,
            duration_ms: milliseconds(self.duration),
            ttfb_ms: self.ttfb.map(milliseconds),
            stdout_bytes: self.stdout,
            stderr_bytes: self.stderr,
            stdout_sha256: self
                .stdout_sha256
                .clone()
                .or_else(|| hash_stdout.then(|| crate::report::sha256(b""))),
        }
    }
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
