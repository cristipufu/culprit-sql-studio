//! Recover one committed incident baseline before native collection starts; no SQL connection is involved.
use super::{
    evidence::RowReceipt,
    recovery::{self, ActivityHead, CommittedRange, RecoveryRecords, RecoveryStore},
};
use culprit_core::{Result, telemetry::*};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub struct ActivityRestoreBudget {
    pub max_states: u32,
    pub max_bytes: u64,
}
impl Default for ActivityRestoreBudget {
    fn default() -> Self {
        let source = CollectionBudget::default();
        Self { max_states: source.max_snapshot_rows, max_bytes: u64::from(source.total_bytes) }
    }
}
/// Constructed only after complete native object, membership, capture and checkpoint validation.
pub struct RecoveredActivityCheckpoint {
    range: CommittedRange,
    states: Vec<ActivityIncidentState>,
}
impl RecoveredActivityCheckpoint {
    pub fn range(&self) -> &CommittedRange {
        &self.range
    }
    pub fn states(&self) -> &[ActivityIncidentState] {
        &self.states
    }
    pub(crate) fn into_parts(self) -> (CommittedRange, Vec<ActivityIncidentState>) {
        (self.range, self.states)
    }
}
pub async fn load_activity_checkpoint(
    store: &dyn RecoveryStore,
    head: &ActivityHead,
    budget: ActivityRestoreBudget,
) -> Result<RecoveredActivityCheckpoint> {
    head.validate()?;
    if budget.max_states == 0
        || budget.max_states > 100_000
        || budget.max_bytes == 0
        || budget.max_bytes > super::contract::MAX_RANGE_BYTES
    {
        return Err(DeliveryProblem::Capacity.error());
    }
    let range = recovery::reopen(store, &head.range_id).await?;
    let manifest = range.manifest();
    let source = &manifest.source;
    if head.manifest_digest != range.manifest_digest()
        || source.logical_source_id()? != head.logical_source_id
        || source.captured_range()?.scope_id != head.scope_id
        || manifest.capture.revision.as_ref() != Some(&head.revision)
    {
        return Err(invalid());
    }
    let snapshot = source
        .observation
        .activity()
        .filter(|snapshot| snapshot.is_valid() && snapshot.complete_scope)
        .ok_or_else(invalid)?;
    let batch = manifest
        .batches
        .iter()
        .find(|batch| batch.stream == TelemetryStream::ActivityCheckpoints && !batch.delivery_required)
        .ok_or_else(invalid)?;
    if batch.bytes > budget.max_bytes || batch.rows.len() > budget.max_states as usize {
        return Err(DeliveryProblem::Capacity.error());
    }
    if batch.rows.len() < snapshot.qualifying as usize {
        return Err(invalid());
    }
    let mut records = RecoveryRecords::open(store, batch).await?;
    let mut states = Vec::with_capacity(batch.rows.len());
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    while let Some(row) = records.next().await? {
        let expected = batch.rows.get(states.len()).ok_or_else(invalid)?;
        if row.capture != manifest.capture
            || row.batch_id != batch.batch_id
            || RowReceipt::from_export(&row)? != *expected
        {
            return Err(invalid());
        }
        let TelemetryRecord::ActivityCheckpoint(state) = row.record else {
            return Err(invalid());
        };
        if !state.is_valid_at(&snapshot.at)
            || !ids.insert(state.id.clone())
            || !keys.insert(state.observation.key.clone())
        {
            return Err(invalid());
        }
        states.push(state);
    }
    // Even an empty checkpoint must reach verified native EOF. A failed read cannot clear older incidents.
    if states.len() != batch.rows.len() {
        return Err(invalid());
    }
    Ok(RecoveredActivityCheckpoint { range, states })
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
