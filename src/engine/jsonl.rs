use std::{fmt, sync::Arc};

use minijinja::{
    value::{Enumerator, Object, ObjectExt, ObjectRepr},
    Value as TemplateValue,
};
use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;

use super::budget::{Budget, Buffer, MAX_STEP_OUTPUT_BYTES, MAX_WORKFLOW_MEMORY_BYTES};
use crate::error::Error;

pub(super) struct Collector<'a> {
    budget: &'a Budget,
    step: &'a str,
    pending: Buffer,
    read: usize,
    line: usize,
    capture: bool,
}

impl<'a> Collector<'a> {
    pub(super) fn new(budget: &'a Budget, step: &'a str, capture: bool) -> Self {
        Self {
            budget,
            step,
            pending: budget.buffer(),
            read: 0,
            line: 1,
            capture,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8], output: &mut Buffer) -> Result<(), Error> {
        if self.capture {
            self.read += bytes.len();
            if self.read > MAX_STEP_OUTPUT_BYTES {
                return Err(Error::MemoryLimitExceeded {
                    step: self.step.to_owned(),
                    limit_bytes: MAX_STEP_OUTPUT_BYTES,
                });
            }
        }
        for part in bytes.split_inclusive(|&byte| byte == b'\n') {
            let line_len =
                self.pending.bytes().len() + part.strip_suffix(b"\n").unwrap_or(part).len();
            if !self.capture && line_len > super::pipe::CHUNK_SIZE {
                return Err(Error::MemoryLimitExceeded {
                    step: self.step.to_owned(),
                    limit_bytes: super::pipe::CHUNK_SIZE,
                });
            }
            if let Some(line) = part.strip_suffix(b"\n") {
                if self.pending.bytes().is_empty() {
                    normalize(self.step, self.line, line, output)?;
                } else {
                    self.pending.append(line, self.step)?;
                    let pending = std::mem::replace(&mut self.pending, self.budget.buffer());
                    normalize(self.step, self.line, pending.bytes(), output)?;
                }
                self.line += 1;
            } else {
                self.pending.append(part, self.step)?;
            }
        }
        Ok(())
    }

    pub(super) fn finish(self, output: &mut Buffer) -> Result<(), Error> {
        normalize(self.step, self.line, self.pending.bytes(), output)
    }
}

fn normalize(step: &str, number: usize, line: &[u8], output: &mut Buffer) -> Result<(), Error> {
    if line.iter().all(|byte| matches!(byte, b' ' | b'\t' | b'\r')) {
        return Ok(());
    }
    validate(line, output.budget(), step, number)?;
    let start = output.bytes().len();
    output
        .resize_zeroed(start + compact(line).count() + 1)
        .map_err(|source| output.stream_error(step, source))?;
    for (target, byte) in output.bytes_mut()[start..].iter_mut().zip(compact(line)) {
        *target = byte;
    }
    *output.bytes_mut().last_mut().expect("newline slot") = b'\n';
    Ok(())
}

fn compact(bytes: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let (mut quoted, mut escaped) = (false, false);
    bytes.iter().copied().filter(move |&byte| {
        if escaped {
            escaped = false;
            return true;
        }
        if byte == b'"' {
            quoted = !quoted;
            return true;
        }
        escaped = quoted && byte == b'\\';
        quoted || !matches!(byte, b' ' | b'\t' | b'\r')
    })
}

fn validate(bytes: &[u8], budget: &Budget, step: &str, line: usize) -> Result<(), Error> {
    // The slice decoder borrows unescaped strings. Reserve room for its growing
    // scratch buffer when a record contains escapes.
    let scratch = if bytes.contains(&b'\\') {
        bytes.len() * 2
    } else {
        0
    };
    if !budget.reserve(scratch) {
        return Err(Error::MemoryLimitExceeded {
            step: step.to_owned(),
            limit_bytes: MAX_WORKFLOW_MEMORY_BYTES,
        });
    }
    let result = serde_json::from_slice::<Valid>(bytes);
    budget.release(scratch);
    result.map(|_| ()).map_err(|source| Error::JsonLine {
        step: step.to_owned(),
        line,
        source,
    })
}

// Validate without constructing an in-memory tree for every record.
struct Valid;

impl<'de> Deserialize<'de> for Valid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Validate)
    }
}

struct Validate;

impl<'de> Visitor<'de> for Validate {
    type Value = Valid;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<Valid, E> {
        Ok(Valid)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Valid, E> {
        Ok(Valid)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Valid, E> {
        Ok(Valid)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Valid, E> {
        Ok(Valid)
    }
    fn visit_str<E>(self, _: &str) -> Result<Valid, E> {
        Ok(Valid)
    }
    fn visit_unit<E>(self) -> Result<Valid, E> {
        Ok(Valid)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Valid, A::Error> {
        while sequence.next_element::<Valid>()?.is_some() {}
        Ok(Valid)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Valid, A::Error> {
        while map.next_entry::<Valid, Valid>()?.is_some() {}
        Ok(Valid)
    }
}

// Retain the monitored byte stream, not a second in-memory array of JSON trees.
pub(super) struct Records {
    stdout: Arc<Buffer>,
    len: usize,
}

impl Records {
    pub(super) fn new(stdout: Arc<Buffer>, step: &str, cached: bool) -> Result<Self, Error> {
        let len = lines(stdout.bytes()).count();
        if cached {
            if !stdout.bytes().is_empty() && !stdout.bytes().ends_with(b"\n") {
                return Err(Error::InvalidWorkflow(
                    "Cached JSONL is missing its final newline".into(),
                ));
            }
            for (index, line) in lines(stdout.bytes()).enumerate() {
                validate(line, stdout.budget(), step, index + 1)?;
            }
        }
        Ok(Self { stdout, len })
    }
}

impl fmt::Debug for Records {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonlRecords")
            .field("len", &self.len)
            .finish()
    }
}

impl Object for Records {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Seq
    }

    fn get_value(self: &Arc<Self>, key: &TemplateValue) -> Option<TemplateValue> {
        lines(self.stdout.bytes())
            .nth(key.as_usize()?)
            .map(template_value)
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        self.mapped_enumerator(|records| {
            Box::new(lines(records.stdout.bytes()).map(template_value))
        })
    }

    fn enumerator_len(self: &Arc<Self>) -> Option<usize> {
        Some(self.len)
    }
}

fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split_inclusive(|&byte| byte == b'\n')
}

fn template_value(bytes: &[u8]) -> TemplateValue {
    let value: Value = serde_json::from_slice(bytes).expect("JSONL records are validated");
    TemplateValue::from_serialize(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_quoted_whitespace_and_normalizes_records_across_reads() {
        let budget = Budget::default();
        let mut output = budget.buffer();
        let mut collector = Collector::new(&budget, "sample", true);
        for chunk in [
            b"  { \"reading\" : \"escaped \\".as_slice(),
            b"\"value  \" }\r",
            b"\n \t\n [1, 2]",
        ] {
            collector.push(chunk, &mut output).unwrap();
        }
        collector.finish(&mut output).unwrap();
        assert_eq!(
            output.bytes(),
            b"{\"reading\":\"escaped \\\"value  \"}\n[1,2]\n"
        );
    }
}
