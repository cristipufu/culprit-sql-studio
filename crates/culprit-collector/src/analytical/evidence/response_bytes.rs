//! A decoded response is bounded without allocating a second JSON document.
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::Serialize;
use std::io::{self, Write};

pub(super) fn count(value: &impl Serialize, limit: usize) -> Result<usize> {
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| DeliveryProblem::Capacity.error())?;
    Ok(counter.bytes)
}

struct Counter {
    bytes: usize,
    limit: usize,
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|count| *count <= self.limit)
            .ok_or(io::ErrorKind::OutOfMemory)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
