//! Bounded provisional row retention for an independently verified Outbox read.
use culprit_core::telemetry::*;

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
