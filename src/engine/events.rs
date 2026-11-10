use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use serde::Serialize;
use tokio::sync::mpsc;

use super::telemetry::StepTelemetry;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum EngineEvent {
    LayerStarted { layer: usize },
    StepStarted { id: String, layer: usize },
    StepStdoutChunk { id: String, chunk: Vec<u8> },
    StepFinished { telemetry: StepTelemetry },
    WorkflowFinished { result: serde_json::Value },
}

const MAX_PREVIEW_EVENTS: usize = 256;
const MAX_PREVIEW_BYTES: usize = 512 * 1024;

#[derive(Clone)]
pub(crate) struct Events(Arc<Sink>);

struct Sink {
    tx: mpsc::UnboundedSender<EngineEvent>,
    chunks: AtomicUsize,
    bytes: AtomicUsize,
    truncated: AtomicBool,
}

impl Events {
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<EngineEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self(Arc::new(Sink {
                tx,
                chunks: AtomicUsize::new(0),
                bytes: AtomicUsize::new(0),
                truncated: AtomicBool::new(false),
            })),
            rx,
        )
    }

    pub(crate) fn emit(&self, event: EngineEvent) {
        // Studio runs with graph bounds, so control events are bounded by the graph.
        let _ = self.0.tx.send(event);
    }

    pub(super) fn stdout(&self, id: &str, bytes: &[u8]) {
        if bytes.is_empty() || self.0.tx.is_closed() {
            return;
        }
        let index = self.0.chunks.fetch_add(1, Ordering::Relaxed);
        let offset = self.0.bytes.fetch_add(bytes.len(), Ordering::Relaxed);
        let count = MAX_PREVIEW_BYTES.saturating_sub(offset).min(bytes.len());
        if index >= MAX_PREVIEW_EVENTS || count < bytes.len() {
            self.0.truncated.store(true, Ordering::Relaxed);
        }
        if index < MAX_PREVIEW_EVENTS && count > 0 {
            self.emit(EngineEvent::StepStdoutChunk {
                id: id.to_owned(),
                chunk: bytes[..count].to_vec(),
            });
        }
    }

    pub(crate) fn truncated(&self) -> bool {
        self.0.truncated.load(Ordering::Relaxed)
    }
}
