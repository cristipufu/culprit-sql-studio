//! Recover only rows whose warehouse projection was authenticated but cannot carry the full native representation.
use super::*;
use crate::delivery::{
    evidence::BatchManifest,
    recovery::{CommittedRange, RecoveryRecords, RecoveryStore},
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::collections::{BTreeMap, BTreeSet};

struct SelectedBatch<'a> {
    batch: &'a BatchManifest,
    ordinals: BTreeSet<usize>,
}

pub(super) async fn restore(
    range: &CommittedRange,
    recovery: &dyn RecoveryStore,
    budget: RecoveryPayloadBudget,
    capture: &mut EvidenceCapture,
) -> Result<()> {
    budget.validate()?;
    if recovery.id() != range.store_id() {
        return Err(DeliveryProblem::DestinationChanged.error());
    }
    let selected = selected_batches(range, &capture.gaps)?;
    preflight(&selected, budget)?;
    let mut recovered = Vec::new();
    let mut repaired = BTreeSet::new();
    for selected in selected {
        capture.metrics.recovery_objects = capture.metrics.recovery_objects.checked_add(1).ok_or_else(capacity)?;
        let rows = read_batch(range, recovery, &selected).await?;
        // Only a verified EOF permits any row from this object to leave read_batch. Keep all objects staged until
        // the complete request succeeds, so a corrupt later object cannot install a partial repair either.
        capture.metrics.recovery_bytes =
            capture.metrics.recovery_bytes.checked_add(selected.batch.bytes).ok_or_else(capacity)?;
        for row in rows {
            repaired.insert((row.batch_id.clone(), row.row_ordinal));
            recovered.push(row);
        }
    }
    if capture.records.iter().any(|row| repaired.contains(&(row.batch_id.clone(), row.row_ordinal))) {
        return Err(invalid());
    }
    tokio::task::yield_now().await;
    capture.records.extend(recovered);
    capture.gaps.retain(|gap| {
        !eligible(gap.reason)
            || !gap
                .batch_id
                .as_ref()
                .zip(gap.row_ordinal)
                .is_some_and(|(batch, ordinal)| repaired.contains(&(batch.clone(), ordinal)))
    });
    Ok(())
}

fn selected_batches<'a>(range: &'a CommittedRange, gaps: &[EvidenceGap]) -> Result<Vec<SelectedBatch<'a>>> {
    let mut selected = BTreeMap::<&str, SelectedBatch<'_>>::new();
    for gap in gaps.iter().filter(|gap| eligible(gap.reason)) {
        let id = gap.batch_id.as_deref().ok_or_else(invalid)?;
        let batch = range.manifest().batches.iter().find(|batch| batch.batch_id == id).ok_or_else(invalid)?;
        let ordinal = gap.row_ordinal.ok_or_else(invalid)? as usize;
        if gap.stream != Some(batch.stream)
            || !batch.delivery_required
            || !batch.stream.delivery_required()
            || ordinal == 0
            || ordinal > batch.rows.len()
        {
            return Err(invalid());
        }
        selected
            .entry(&batch.batch_id)
            .or_insert_with(|| SelectedBatch { batch, ordinals: BTreeSet::new() })
            .ordinals
            .insert(ordinal);
    }
    Ok(selected.into_values().collect())
}

fn preflight(selected: &[SelectedBatch<'_>], budget: RecoveryPayloadBudget) -> Result<()> {
    if selected.len() > budget.max_objects as usize {
        return Err(capacity());
    }
    selected.iter().try_fold(0u64, |total, selected| {
        total.checked_add(selected.batch.bytes).filter(|bytes| *bytes <= budget.max_bytes).ok_or_else(capacity)
    })?;
    Ok(())
}

async fn read_batch(
    range: &CommittedRange,
    recovery: &dyn RecoveryStore,
    selected: &SelectedBatch<'_>,
) -> Result<Vec<VerifiedEvidenceRecord>> {
    let manifest = range.manifest();
    let batch = selected.batch;
    let mut stream = RecoveryRecords::open(recovery, batch).await?;
    let mut ordinal = 0usize;
    let mut wanted = Vec::with_capacity(selected.ordinals.len());
    while let Some(row) = stream.next().await? {
        ordinal += 1;
        batch.validate_row(&manifest.source, &manifest.capture, ordinal, &row)?;
        // Buffered AsyncRead implementations may otherwise parse the entire object without returning Pending.
        // Yield at a bounded native-row boundary so the one read deadline and caller cancellation can progress.
        tokio::task::yield_now().await;
        if selected.ordinals.contains(&ordinal) {
            wanted.push(VerifiedEvidenceRecord {
                batch_id: batch.batch_id.clone(),
                row_ordinal: ordinal as u32,
                native_payload_sha256: batch.rows[ordinal - 1].payload_digest.clone(),
                row,
            });
        }
    }
    if ordinal != batch.rows.len() || wanted.len() != selected.ordinals.len() {
        return Err(invalid());
    }
    Ok(wanted)
}

fn eligible(reason: EvidenceGapReason) -> bool {
    matches!(reason, EvidenceGapReason::DetailInRecovery | EvidenceGapReason::NativeRepresentationRequired)
}
fn capacity() -> culprit_core::Error {
    DeliveryProblem::Capacity.error()
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests;
