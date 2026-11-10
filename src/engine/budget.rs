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
pub(super) struct Budget(Arc<AtomicUsize>);

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
        let mut used = self.0.load(Ordering::Relaxed);
        loop {
            if bytes > MAX_WORKFLOW_MEMORY_BYTES - used {
                return false;
            }
            match self.0.compare_exchange_weak(
                used,
                used + bytes,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(current) => used = current,
            }
        }
    }

    pub(super) fn release(&self, bytes: usize) {
        self.0.fetch_sub(bytes, Ordering::Relaxed);
    }
}

pub(super) struct Buffer {
    bytes: Vec<u8>,
    budget: Budget,
    reserved: usize,
    exceeded: Option<usize>,
}

impl Buffer {
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    pub(super) fn limit_error(&self, step: &str) -> Option<Error> {
        self.exceeded.map(|limit_bytes| Error::MemoryLimitExceeded {
            step: step.to_owned(),
            limit_bytes,
        })
    }
}

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let required = self.bytes.len().saturating_add(bytes.len());
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
        if let Err(error) = self.bytes.try_reserve_exact(additional) {
            self.budget.release(additional);
            return Err(io::Error::other(error));
        }
        self.reserved += additional;
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
