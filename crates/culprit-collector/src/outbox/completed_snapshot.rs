//! Local byte authentication uses the existing Outbox owner. It never opens a database or grants cost approval.
use super::{
    OutboxDelivery,
    completed_capture::{CompletedCaptureBudget, CompletedCaptureRead},
    problem,
};
#[cfg(test)]
use super::{artifacts, completed_capture::ReadControl};
use crate::capture::CollectionCancellation;
#[cfg(test)]
use culprit_core::ErrorDetail;
use culprit_core::{Error, Result, telemetry::*};
#[cfg(test)]
use std::time::Duration;

/// Authenticated local originals, deliberately distinct from an archive EvidenceCapture. Construction is private;
/// only the original Outbox journal/manifest/file verifier may publish this value. Structural receipts remain
/// valid after this value is read, even if the independently owned local files later change or are evicted.
#[derive(Debug)]
pub struct CompletedSnapshot {
    source: CollectionDescription,
    capture: DiagnosticCaptureRef,
    records: Vec<ExportedRecord>,
    row_ordinals: Vec<u32>,
    bytes_read: u64,
}
impl CompletedSnapshot {
    pub fn source(&self) -> &CollectionDescription {
        &self.source
    }
    pub fn capture(&self) -> &DiagnosticCaptureRef {
        &self.capture
    }
    pub fn records(&self) -> &[ExportedRecord] {
        &self.records
    }
    pub fn row_ordinals(&self) -> &[u32] {
        &self.row_ordinals
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    /// The local read returns every selected original row or fails. Native coverage remains in the source.
    pub fn is_read_complete(&self) -> bool {
        true
    }
    pub fn reference(&self, index: usize) -> Result<DiagnosticEvidenceRef> {
        let row = self.records.get(index).ok_or_else(integrity)?;
        let ordinal = *self.row_ordinals.get(index).ok_or_else(integrity)?;
        DiagnosticEvidenceRef::from_export(&self.source, &self.capture.manifest_sha256, row, ordinal)
    }
    /// Move the original rows once into the shared model; no fake native read or warehouse wrapper is introduced.
    pub fn into_cached_runtime_read(self) -> Result<CachedRuntimeRead> {
        if !self.source.observation.snapshot().is_some_and(|header| header.dataset == SnapshotDataset::CachedRuntime) {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let positions = self
            .records
            .iter()
            .zip(&self.row_ordinals)
            .map(|(row, ordinal)| CachedRuntimeCapturedPosition::from_export(row, *ordinal))
            .collect::<Result<Vec<_>>>()?;
        let origin = CachedRuntimeReadOrigin::captured(&self.source, self.capture, positions)?;
        let observations = self
            .records
            .into_iter()
            .map(|row| match row.record {
                TelemetryRecord::Operational(row) => Ok(row),
                _ => Err(integrity()),
            })
            .collect::<Result<Vec<_>>>()?;
        let read = CachedRuntimeRead { origin, description: self.source, observations, read_complete: true };
        if !read.is_valid() {
            return Err(integrity());
        }
        Ok(read)
    }
}

impl OutboxDelivery {
    /// One absolute caller deadline includes queued ownership and the complete local verification pass.
    /// Cancellation retains the blocking worker's ownership until it stops; no source work is started or cancelled.
    pub async fn completed_snapshot(
        &self,
        attempt: &DiagnosticAttemptReceipt,
        dataset: SnapshotDataset,
        budget: CompletedCaptureBudget,
        cancellation: &CollectionCancellation,
    ) -> Result<CompletedSnapshot> {
        let read = CompletedCaptureRead::snapshot(attempt.clone(), dataset, budget, cancellation.clone())?;
        self.with_completed_capture(read, |completed| {
            Ok(CompletedSnapshot {
                source: completed.source,
                capture: completed.capture,
                records: completed.records,
                row_ordinals: completed.row_ordinals,
                bytes_read: completed.bytes_read,
            })
        })
        .await
    }
}

fn integrity() -> Error {
    problem(OutboxProblem::Integrity)
}

#[cfg(test)]
mod tests;
