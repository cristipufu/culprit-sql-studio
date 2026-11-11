//! Bounded pages over an already completed native observation. Destination backpressure retains no native lock.
use super::*;
use std::collections::VecDeque;
const ENVELOPE_ALLOWANCE: usize = 1024;
pub struct SnapshotBuffer {
    rows: VecDeque<SnapshotRecord>,
    budget: CollectionBudget,
}
impl SnapshotBuffer {
    pub fn new(rows: Vec<SnapshotRecord>, budget: &CollectionBudget) -> Result<Self> {
        budget.validate()?;
        if rows.len() > budget.max_snapshot_rows as usize {
            return Err(CollectionStop::RowLimit.error());
        }
        let bytes = rows.iter().try_fold(0usize, |sum, row| {
            serde_json::to_vec(&row.record)
                .ok()
                .and_then(|bytes| sum.checked_add(bytes.len()))
                .ok_or_else(invalid_source)
        })?;
        if bytes > budget.total_bytes as usize {
            return Err(CollectionStop::ByteLimit.error());
        }
        Ok(Self { rows: rows.into(), budget: budget.clone() })
    }
    pub fn next_batch(&mut self) -> Result<ReadBatch<SnapshotRecord>> {
        let mut rows = vec![];
        let mut bytes = 0usize;
        while rows.len() < self.budget.page_rows as usize {
            let Some(next) = self.rows.front() else {
                break;
            };
            let size = serde_json::to_vec(&next.record).map_err(|_| invalid_source())?.len() + ENVELOPE_ALLOWANCE;
            if bytes + size > self.budget.page_bytes as usize {
                if rows.is_empty() {
                    return Err(CollectionStop::ByteLimit.error());
                }
                break;
            }
            bytes += size;
            rows.push(self.rows.pop_front().ok_or_else(invalid_source)?);
        }
        Ok(ReadBatch { rows, complete: self.rows.is_empty() })
    }
}
