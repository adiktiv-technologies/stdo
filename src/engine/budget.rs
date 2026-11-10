use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use crate::error::Error;

pub const MAX_STEP_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_WORKFLOW_MEMORY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Default)]
pub(super) struct Budget(Arc<Usage>);

#[derive(Default)]
struct Usage {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl Budget {
    pub(super) fn buffer(&self) -> Buffer {
        Buffer {
            bytes: Vec::new(),
            budget: self.clone(),
            reserved: 0,
            exceeded: None,
        }
    }

    pub(super) fn reserve(&self, bytes: usize) -> bool {
        let mut used = self.0.current.load(Ordering::Relaxed);
        loop {
            if bytes > MAX_WORKFLOW_MEMORY_BYTES - used {
                return false;
            }
            match self.0.current.compare_exchange_weak(
                used,
                used + bytes,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.0.peak.fetch_max(used + bytes, Ordering::Relaxed);
                    return true;
                }
                Err(current) => used = current,
            }
        }
    }

    pub(super) fn release(&self, bytes: usize) {
        self.0.current.fetch_sub(bytes, Ordering::Relaxed);
    }

    pub(super) fn peak_bytes(&self) -> usize {
        self.0.peak.load(Ordering::Relaxed)
    }
}

pub(super) struct Buffer {
    bytes: Vec<u8>,
    budget: Budget,
    reserved: usize,
    exceeded: Option<usize>,
}

impl Buffer {
    pub(super) fn budget(&self) -> &Budget {
        &self.budget
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    pub(super) fn clear(&mut self) {
        self.bytes.clear();
    }

    pub(super) fn limit_error(&self, step: &str) -> Option<Error> {
        self.exceeded.map(|limit_bytes| Error::MemoryLimitExceeded {
            step: step.to_owned(),
            limit_bytes,
        })
    }

    pub(super) fn stream_error(&self, step: &str, source: io::Error) -> Error {
        self.limit_error(step).unwrap_or_else(|| Error::Process {
            step: step.to_owned(),
            source,
        })
    }

    pub(super) fn append(&mut self, bytes: &[u8], step: &str) -> Result<(), Error> {
        self.write_all(bytes)
            .map_err(|source| self.stream_error(step, source))
    }
    pub(super) fn resize_zeroed(&mut self, size: usize) -> io::Result<()> {
        self.reserve(size)?;
        self.bytes.resize(size, 0);
        Ok(())
    }

    fn reserve(&mut self, required: usize) -> io::Result<()> {
        let additional = required.saturating_sub(self.reserved);
        let limit = if required > MAX_STEP_OUTPUT_BYTES {
            Some(MAX_STEP_OUTPUT_BYTES)
        } else if !self.budget.reserve(additional) {
            Some(MAX_WORKFLOW_MEMORY_BYTES)
        } else {
            None
        };
        if let Some(limit) = limit {
            self.exceeded = Some(limit);
            return Err(io::Error::other("workflow buffer limit exceeded"));
        }
        if let Err(error) = self
            .bytes
            .try_reserve_exact(required.saturating_sub(self.bytes.len()))
        {
            self.budget.release(additional);
            return Err(io::Error::other(error));
        }
        self.reserved += additional;
        Ok(())
    }
}

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.reserve(self.bytes.len().saturating_add(bytes.len()))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.bytes));
        self.budget.release(self.reserved);
    }
}
