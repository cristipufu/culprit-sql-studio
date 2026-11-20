//! Local byte authentication uses the existing Outbox owner. It never opens a database or grants cost approval.
use super::{
    OutboxDelivery,
    completed_capture::{CompletedCapture, CompletedCaptureBudget, CompletedCaptureLookup, CompletedCaptureRead},
    problem,
};
#[cfg(test)]
use super::{artifacts, completed_capture::ReadControl};
use crate::capture::CollectionCancellation;
#[cfg(test)]
use culprit_core::ErrorDetail;
use culprit_core::{Error, Result, limits::OperationHandle, telemetry::*};
#[cfg(test)]
use std::time::Duration;

/// Authenticated local originals, deliberately distinct from an archive EvidenceCapture. Construction is private;
/// only the original Outbox journal/manifest/file verifier may publish this value. Structural receipts remain
/// valid after this value is read, even if the independently owned local files later change or are evicted.
#[derive(Debug)]
pub struct CompletedSnapshot {
    connection: String,
    source: CollectionDescription,
    original: CapturedSnapshotOrigin,
    records: Vec<ExportedRecord>,
    row_ordinals: Vec<u32>,
    collected_from: String,
    collected_until: String,
    bytes_read: u64,
}
impl CompletedSnapshot {
    /// The saved journal association, independently checked by connection-bound readers.
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn source(&self) -> &CollectionDescription {
        &self.source
    }
    pub fn capture(&self) -> &DiagnosticCaptureRef {
        &self.original.capture
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
    pub fn collected_from(&self) -> &str {
        &self.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.collected_until
    }
    /// The local read returns every selected original row or fails. Native coverage remains in the source.
    pub fn is_read_complete(&self) -> bool {
        true
    }
    pub fn reference(&self, index: usize) -> Result<DiagnosticEvidenceRef> {
        let position = self.original_position(index)?.clone();
        let row = self.records.get(index).ok_or_else(integrity)?;
        // Select the unchanged original position; the shared validator checks its source and canonical body.
        let original = CapturedSnapshotOrigin {
            capture: self.original.capture.clone(),
            source_sha256: self.original.source_sha256.clone(),
            positions: vec![position],
        };
        original
            .references(&self.source, [row.record.clone()], false)
            .map_err(|_| integrity())?
            .pop()
            .ok_or_else(integrity)
    }
    /// Move the original rows once into the shared model; no fake native read or warehouse wrapper is introduced.
    pub fn into_cached_runtime_read(self) -> Result<CachedRuntimeRead> {
        if !self.source.observation.snapshot().is_some_and(|header| header.dataset == SnapshotDataset::CachedRuntime) {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        self.validate_original_positions()?;
        let origin = CachedRuntimeReadOrigin::Captured {
            capture: self.original.capture,
            source_sha256: self.original.source_sha256,
            positions: self.original.positions,
        };
        let observations = operational_observations(self.records)?;
        let read = CachedRuntimeRead { origin, description: self.source, observations, read_complete: true };
        if !read.is_valid() {
            return Err(integrity());
        }
        Ok(read)
    }

    /// Retain the whole original Operational dataset, including unrelated source rows and coverage.
    pub fn into_operational_read(self) -> Result<CapturedOperationalRead> {
        if !self
            .source
            .observation
            .snapshot()
            .is_some_and(|header| header.dataset.stream() == TelemetryStream::OperationalSamples)
        {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        self.validate_original_positions()?;
        let observations = operational_observations(self.records)?;
        let read = CapturedOperationalRead {
            origin: self.original,
            description: self.source,
            observations,
            read_complete: true,
        };
        read.is_valid().then_some(read).ok_or_else(integrity)
    }

    fn original_position(&self, index: usize) -> Result<&CapturedSnapshotPosition> {
        let position = self.original_positions()?.get(index).ok_or_else(integrity)?;
        let row = self.records.get(index).ok_or_else(integrity)?;
        if self.row_ordinals[index] != position.row_ordinal || row.payload_sha256()? != position.payload_sha256 {
            return Err(integrity());
        }
        Ok(position)
    }

    fn original_positions(&self) -> Result<&[CapturedSnapshotPosition]> {
        if self.records.len() != self.row_ordinals.len() || self.records.len() != self.original.positions.len() {
            return Err(integrity());
        }
        Ok(&self.original.positions)
    }

    fn validate_original_positions(&self) -> Result<()> {
        self.original_positions()?;
        for index in 0..self.records.len() {
            self.original_position(index)?;
        }
        Ok(())
    }

    fn from_completed(completed: CompletedCapture) -> Result<Self> {
        if completed.records.len() != completed.row_ordinals.len() {
            return Err(integrity());
        }
        // Still inside the canonical read worker: bind its verified original once, before any projection.
        // Conversion moves this proof unchanged rather than hashing a later body or repairing its descriptor.
        let positions = completed
            .records
            .iter()
            .zip(&completed.row_ordinals)
            .map(|(row, ordinal)| CapturedSnapshotPosition::from_export(row, *ordinal))
            .collect::<Result<Vec<_>>>()?;
        let original = CapturedSnapshotOrigin::captured(&completed.source, completed.capture, positions)?;
        Ok(Self {
            connection: completed.connection,
            source: completed.source,
            original,
            records: completed.records,
            row_ordinals: completed.row_ordinals,
            collected_from: completed.collected_from,
            collected_until: completed.collected_until,
            bytes_read: completed.bytes_read,
        })
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
        self.with_completed_capture(read, (), CompletedSnapshot::from_completed).await
    }

    /// Authenticate both the original receipt and its saved connection before exposing any retained rows.
    pub async fn completed_snapshot_for_connection(
        &self,
        connection: &str,
        attempt: &DiagnosticAttemptReceipt,
        dataset: SnapshotDataset,
        budget: CompletedCaptureBudget,
        cancellation: &CollectionCancellation,
    ) -> Result<CompletedSnapshot> {
        let read = CompletedCaptureRead::snapshot(attempt.clone(), dataset, budget, cancellation.clone())?
            .for_connection(connection)?;
        self.with_completed_capture(read, (), CompletedSnapshot::from_completed).await
    }

    /// The same worker owns lookup, authentication and projection under the caller's original operation.
    /// Its lifetime guard is released after exclusive Files access, including postcheck failure and unwind.
    pub async fn with_completed_snapshot<T: Send + 'static>(
        &self,
        lookup: CompletedCaptureLookup<'_>,
        dataset: SnapshotDataset,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(CompletedSnapshot) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.with_completed_snapshots(lookup, &[dataset], budget, operation, worker_guard, project).await
    }

    /// An explicit logical analysis may accept several native datasets, authenticated by the same original worker.
    /// The nonempty selection must be unique and retain one stream layout; native scope and rows remain unchanged.
    pub async fn with_completed_snapshots<T: Send + 'static>(
        &self,
        lookup: CompletedCaptureLookup<'_>,
        datasets: &[SnapshotDataset],
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(CompletedSnapshot) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let read = CompletedCaptureRead::snapshot_lookup(lookup, datasets, budget, operation)?;
        self.with_completed_capture(read, worker_guard, move |completed| {
            project(CompletedSnapshot::from_completed(completed)?)
        })
        .await
    }
}

fn integrity() -> Error {
    problem(OutboxProblem::Integrity)
}

fn operational_observations(records: Vec<ExportedRecord>) -> Result<Vec<OperationalObservation>> {
    records
        .into_iter()
        .map(|row| match row.record {
            TelemetryRecord::Operational(row) => Ok(row),
            _ => Err(integrity()),
        })
        .collect()
}

#[cfg(test)]
mod tests;
