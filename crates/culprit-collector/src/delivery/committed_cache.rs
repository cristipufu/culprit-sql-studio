//! Bounded process-local capabilities. Every cache miss reopens the authoritative recovery store.
use super::recovery::{self, CommittedRange, RecoveryStore};
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::collections::BTreeMap;

const MAX_ENTRIES: usize = 64;
const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub struct CommittedRangeCache {
    entries: BTreeMap<(String, String), Entry>,
    encoded_bytes: usize,
    clock: u64,
}
struct Entry {
    range: CommittedRange,
    encoded_bytes: usize,
    used: u64,
}
impl CommittedRangeCache {
    pub(crate) async fn get(
        &mut self,
        store: &dyn RecoveryStore,
        id: &str,
        expected_digest: &str,
        encoded_bytes: usize,
    ) -> Result<CommittedRange> {
        let key = (store.id().to_owned(), id.to_owned());
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.range.manifest_digest() != expected_digest || entry.encoded_bytes != encoded_bytes {
                return Err(DeliveryProblem::Integrity.error());
            }
            entry.used = self.clock;
            return Ok(entry.range.clone());
        }
        let range = recovery::reopen(store, id).await?;
        if range.manifest_digest() != expected_digest {
            return Err(DeliveryProblem::Integrity.error());
        }
        if encoded_bytes > MAX_ENCODED_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        while self.entries.len() >= MAX_ENTRIES || self.encoded_bytes + encoded_bytes > MAX_ENCODED_BYTES {
            let oldest = self.entries.iter().min_by_key(|(_, entry)| entry.used).map(|(key, _)| key.clone());
            let key = oldest.ok_or_else(|| DeliveryProblem::Integrity.error())?;
            let removed = self.entries.remove(&key).ok_or_else(|| DeliveryProblem::Integrity.error())?;
            self.encoded_bytes -= removed.encoded_bytes;
        }
        self.encoded_bytes += encoded_bytes;
        self.entries.insert(key, Entry { range: range.clone(), encoded_bytes, used: self.clock });
        Ok(range)
    }
}
