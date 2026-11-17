//! A verification pass owns its open file and charges bytes before any provisional row can escape.
use super::{integrity, storage};
use culprit_core::{Error, Result, telemetry::*};
use std::{
    fs::File,
    io::{self, Read},
};

pub(in crate::outbox) trait VerificationObserver {
    fn check(&self) -> Result<()> {
        Ok(())
    }
    fn caller_refused(&self, _error: &Error) -> bool {
        false
    }
    fn allowance(&self, requested: usize) -> Result<usize> {
        Ok(requested)
    }
    fn charged(&mut self, _bytes: usize) -> Result<()> {
        Ok(())
    }
    fn manifest(&mut self, _manifest: &CollectionManifest) -> Result<()> {
        Ok(())
    }
    fn row(&mut self, _row: &ExportedRecord, _ordinal: u32) -> Result<()> {
        Ok(())
    }
}

pub(in crate::outbox) struct NoObservation;
impl VerificationObserver for NoObservation {}

/// Canonical rows remain provisional until the outer manifest, file digests and journal receipt all validate.
pub(in crate::outbox) const MAX_CANONICAL_ROWS: u32 = 100_000;

pub(in crate::outbox) struct CanonicalRows {
    rows: Vec<(u32, ExportedRecord)>,
    max_rows: u32,
}
impl CanonicalRows {
    pub fn new(max_rows: u32) -> Self {
        Self { rows: vec![], max_rows }
    }
    pub fn retain(&mut self, row: &ExportedRecord, ordinal: u32) -> bool {
        if self.rows.len() >= self.max_rows as usize || self.rows.len() >= MAX_CANONICAL_ROWS as usize {
            return false;
        }
        self.rows.push((ordinal, row.clone()));
        true
    }
    #[cfg(test)]
    pub fn retained_rows(&self) -> usize {
        self.rows.len()
    }
    pub fn into_parts(self) -> (Vec<ExportedRecord>, Vec<u32>) {
        split_rows(self.rows)
    }
    pub fn drain(&mut self) -> (Vec<ExportedRecord>, Vec<u32>) {
        split_rows(std::mem::take(&mut self.rows))
    }
}
fn split_rows(rows: Vec<(u32, ExportedRecord)>) -> (Vec<ExportedRecord>, Vec<u32>) {
    rows.into_iter().map(|(ordinal, row)| (row, ordinal)).unzip()
}

pub(super) struct ObservedFile<'a> {
    input: File,
    length: u64,
    remaining: u64,
    pub observer: &'a mut dyn VerificationObserver,
}
impl<'a> ObservedFile<'a> {
    pub fn new(input: File, length: u64, observer: &'a mut dyn VerificationObserver) -> Self {
        Self { input, length, remaining: length, observer }
    }
    pub fn finish(&self) -> Result<()> {
        self.observer.check()?;
        if self.remaining != 0 || self.input.metadata().map_err(storage)?.len() != self.length {
            return Err(integrity());
        }
        Ok(())
    }
}
impl Read for ObservedFile<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.observer.check().map_err(io::Error::other)?;
        if self.remaining == 0 || bytes.is_empty() {
            return Ok(0);
        }
        let requested = bytes.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let allowed = self.observer.allowance(requested).map_err(io::Error::other)?.min(requested);
        if allowed == 0 {
            return Err(io::Error::other(DeliveryProblem::Capacity.error()));
        }
        let count = self.input.read(&mut bytes[..allowed])?;
        self.remaining -= count as u64;
        self.observer.charged(count).map_err(io::Error::other)?;
        Ok(count)
    }
}

pub(super) fn read_error(error: io::Error) -> Error {
    error.get_ref().and_then(|error| error.downcast_ref::<Error>()).cloned().unwrap_or_else(|| storage(error))
}
