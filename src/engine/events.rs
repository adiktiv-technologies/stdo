use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
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
    StepStdoutBinary { id: String, format: &'static str },
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
    previews: Mutex<HashMap<String, Preview>>,
}

#[derive(Default)]
struct Preview {
    prefix: Vec<u8>,
    checked_header: bool,
    binary: bool,
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
                previews: Mutex::default(),
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
        let mut previews = self.0.previews.lock().expect("stdout preview lock");
        let preview = previews.entry(id.to_owned()).or_default();
        if preview.binary {
            return;
        }
        let mut bytes = bytes;
        let prefix = if preview.checked_header {
            Vec::new()
        } else {
            // A RIFF signature may be split across reads. Hold at most 12 bytes.
            let count = (12 - preview.prefix.len()).min(bytes.len());
            preview.prefix.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            let prefix = &preview.prefix;
            let possible_riff = b"RIFF".starts_with(&prefix[..prefix.len().min(4)])
                && b"WAVE".starts_with(prefix.get(8..).unwrap_or_default());
            if possible_riff && prefix.len() < 12 {
                return;
            }
            if possible_riff {
                preview.binary = true;
                self.binary(id, "RIFF/WAV audio");
                return;
            }
            preview.checked_header = true;
            std::mem::take(&mut preview.prefix)
        };
        let controls = prefix
            .iter()
            .chain(bytes)
            .filter(|&&byte| {
                (byte < 32 && !matches!(byte, b'\t' | b'\n' | b'\r' | 27)) || byte == 127
            })
            .count();
        if prefix.contains(&0) || bytes.contains(&0) || controls * 10 > prefix.len() + bytes.len() {
            preview.binary = true;
            self.binary(id, "binary data");
        } else {
            self.text(id, &prefix, bytes);
        }
    }

    pub(super) fn finish_stdout(&self, id: &str) {
        let preview = self
            .0
            .previews
            .lock()
            .expect("stdout preview lock")
            .remove(id);
        if let Some(preview) = preview {
            if !preview.binary && !preview.prefix.is_empty() {
                self.text(id, &preview.prefix, &[]);
            }
        }
    }

    fn binary(&self, id: &str, format: &'static str) {
        self.emit(EngineEvent::StepStdoutBinary {
            id: id.to_owned(),
            format,
        });
    }

    fn text(&self, id: &str, prefix: &[u8], bytes: &[u8]) {
        let index = self.0.chunks.fetch_add(1, Ordering::Relaxed);
        let length = prefix.len() + bytes.len();
        let offset = self.0.bytes.fetch_add(length, Ordering::Relaxed);
        let count = MAX_PREVIEW_BYTES.saturating_sub(offset).min(length);
        if index >= MAX_PREVIEW_EVENTS || count < length {
            self.0.truncated.store(true, Ordering::Relaxed);
        }
        if index < MAX_PREVIEW_EVENTS && count > 0 {
            let prefix_count = count.min(prefix.len());
            let mut chunk = Vec::with_capacity(count);
            chunk.extend_from_slice(&prefix[..prefix_count]);
            chunk.extend_from_slice(&bytes[..count - prefix_count]);
            self.emit(EngineEvent::StepStdoutChunk {
                id: id.to_owned(),
                chunk,
            });
        }
    }

    pub(crate) fn truncated(&self) -> bool {
        self.0.truncated.load(Ordering::Relaxed)
    }
}
