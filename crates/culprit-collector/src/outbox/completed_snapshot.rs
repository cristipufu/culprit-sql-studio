//! Local byte authentication uses the existing Outbox owner. It never opens a database or grants cost approval.
use super::{Files, OutboxDelivery, artifacts, problem};
use crate::capture::CollectionCancellation;
use culprit_core::{Error, ErrorDetail, Result, telemetry::*};
use std::time::Duration;
use tokio::time::Instant;

const DEFAULT_ROWS: u32 = 10_000;
const DEFAULT_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ROWS: u32 = artifacts::MAX_CANONICAL_ROWS;
const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TIMEOUT: Duration = Duration::from_secs(300);

/// An independent storage-read allowance; none of these bounds changes native collection or qualification.
#[derive(Clone, Copy, Debug)]
pub struct CompletedSnapshotBudget {
    pub max_rows: u32,
    /// Actual manifest and artifact bytes consumed by the verification pass, not native I/O or decoded value size.
    pub max_bytes: u64,
    pub timeout: Duration,
}
impl Default for CompletedSnapshotBudget {
    fn default() -> Self {
        Self { max_rows: DEFAULT_ROWS, max_bytes: DEFAULT_BYTES, timeout: DEFAULT_TIMEOUT }
    }
}
impl CompletedSnapshotBudget {
    fn validate(self) -> Result<Self> {
        if !(1..=MAX_ROWS).contains(&self.max_rows)
            || !(1..=MAX_BYTES).contains(&self.max_bytes)
            || self.timeout.is_zero()
            || self.timeout > MAX_TIMEOUT
        {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(self)
    }
}

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
        budget: CompletedSnapshotBudget,
        cancellation: &CollectionCancellation,
    ) -> Result<CompletedSnapshot> {
        let budget = budget.validate()?;
        if !attempt.is_valid() || attempt.manifest_sha256.is_none() {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let deadline = Instant::now().checked_add(budget.timeout).ok_or_else(|| CollectionStop::TimedOut.error())?;
        let mut read = SnapshotRead {
            attempt: attempt.clone(),
            dataset,
            budget,
            deadline,
            cancellation: cancellation.clone(),
            rows: artifacts::CanonicalRows::new(budget.max_rows),
            bytes_read: 0,
        };
        read.check()?;
        let access = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(CollectionStop::Cancelled.error()),
            _ = tokio::time::sleep_until(deadline) => return Err(CollectionStop::TimedOut.error()),
            access = self.acquire_files() => access,
        };
        // Once started, the worker is joined. Its cooperative checks retain file and journal ownership on refusal.
        access.run(move |files| files.completed_snapshot(&mut read)).await
    }
}

struct SnapshotRead {
    attempt: DiagnosticAttemptReceipt,
    dataset: SnapshotDataset,
    budget: CompletedSnapshotBudget,
    deadline: Instant,
    cancellation: CollectionCancellation,
    rows: artifacts::CanonicalRows,
    bytes_read: u64,
}
impl artifacts::VerificationObserver for SnapshotRead {
    fn check(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        if Instant::now() >= self.deadline {
            return Err(CollectionStop::TimedOut.error());
        }
        Ok(())
    }
    fn caller_refused(&self, error: &Error) -> bool {
        matches!(
            error.detail,
            Some(ErrorDetail::Delivery(
                DeliveryProblem::Capacity | DeliveryProblem::Conflict | DeliveryProblem::UnsupportedSchema
            )) | Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled | CollectionStop::TimedOut))
        )
    }
    fn allowance(&self, requested: usize) -> Result<usize> {
        self.check()?;
        let remaining =
            self.budget.max_bytes.checked_sub(self.bytes_read).ok_or_else(|| DeliveryProblem::Capacity.error())?;
        Ok(requested.min(usize::try_from(remaining).unwrap_or(usize::MAX)))
    }
    fn charged(&mut self, bytes: usize) -> Result<()> {
        self.bytes_read = self.bytes_read.checked_add(bytes as u64).ok_or_else(|| DeliveryProblem::Capacity.error())?;
        self.check()
    }
    fn manifest(&mut self, manifest: &CollectionManifest) -> Result<()> {
        self.check()?;
        let header = manifest
            .description
            .as_ref()
            .and_then(|source| source.observation.snapshot())
            .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
        if header.dataset != self.dataset {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        if header.records > self.budget.max_rows {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(())
    }
    fn row(&mut self, row: &ExportedRecord, ordinal: u32) -> Result<()> {
        self.check()?;
        if row.record.stream() != self.dataset.stream() || !self.rows.retain(row, ordinal) {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(())
    }
}
use artifacts::VerificationObserver;

impl Files {
    fn completed_snapshot(&mut self, read: &mut SnapshotRead) -> Result<CompletedSnapshot> {
        read.check()?;
        let capture = self.journal.capture(&read.attempt.capture.observation_id)?;
        if capture.identity != read.attempt.capture {
            return Err(DeliveryProblem::Conflict.error());
        }
        if capture.state != OutboxCaptureState::Ready || capture.source_work != SourceWorkState::Quiescent {
            return Err(problem(OutboxProblem::SourceUnconfirmed));
        }
        let validated = self.validated_observed(&capture.identity.observation_id, read)?;
        read.check()?;
        if read.attempt.manifest_sha256.as_deref() != Some(&validated.manifest_sha) {
            return Err(DeliveryProblem::Conflict.error());
        }
        let source = validated.manifest.description.ok_or_else(integrity)?;
        let capture = DiagnosticCaptureRef {
            capture: validated.manifest.capture,
            manifest_sha256: validated.manifest_sha,
            source_scope_id: source.captured_range()?.scope_id,
        };
        if !capture.is_valid() {
            return Err(integrity());
        }
        read.check()?;
        let (records, row_ordinals) = read.rows.drain();
        read.check()?;
        Ok(CompletedSnapshot { source, capture, records, row_ordinals, bytes_read: read.bytes_read })
    }
}

fn integrity() -> Error {
    problem(OutboxProblem::Integrity)
}

#[cfg(test)]
mod tests;
