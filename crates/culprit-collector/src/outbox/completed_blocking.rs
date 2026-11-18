//! A retained closure and its exact original participant are authenticated under one local read owner.
use super::{
    OutboxDelivery,
    completed_capture::{CompletedCapture, CompletedCaptureBudget, CompletedCaptureLookup, CompletedCaptureRead},
};
use culprit_core::{Result, limits::OperationHandle, telemetry::*};

/// Only the canonical Outbox worker constructs these original Activity bytes and positions.
#[derive(Debug)]
pub struct CompletedActivity {
    completed: CompletedCapture,
    references: Vec<DiagnosticEvidenceRef>,
}
impl CompletedActivity {
    fn from_completed(completed: CompletedCapture) -> Result<Self> {
        if completed.records.len() != completed.row_ordinals.len() || completed.source.observation.activity().is_none()
        {
            return Err(invalid());
        }
        let references = completed
            .records
            .iter()
            .zip(&completed.row_ordinals)
            .map(|(row, ordinal)| {
                if !matches!(row.record, TelemetryRecord::Activity(_)) {
                    return Err(invalid());
                }
                DiagnosticEvidenceRef::from_export(&completed.source, &completed.capture.manifest_sha256, row, *ordinal)
            })
            .collect::<Result<Vec<_>>>()?;
        let read = Self { completed, references };
        read.frame()?;
        Ok(read)
    }
    pub fn source(&self) -> &CollectionDescription {
        &self.completed.source
    }
    pub fn capture(&self) -> &DiagnosticCaptureRef {
        &self.completed.capture
    }
    pub fn connection(&self) -> &str {
        &self.completed.connection
    }
    pub fn records(&self) -> &[ExportedRecord] {
        &self.completed.records
    }
    pub fn row_ordinals(&self) -> &[u32] {
        &self.completed.row_ordinals
    }
    pub fn references(&self) -> &[DiagnosticEvidenceRef] {
        &self.references
    }
    pub fn into_rows(self) -> Vec<super::DiagnosticInputRow> {
        self.completed
            .records
            .into_iter()
            .zip(self.references)
            .map(|(row, reference)| super::DiagnosticInputRow { row, reference })
            .collect()
    }
    pub fn collected_from(&self) -> &str {
        &self.completed.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.completed.collected_until
    }
    pub fn bytes_read(&self) -> u64 {
        self.completed.bytes_read
    }
    pub fn frame(&self) -> Result<ActivityFrameEvidence<'_>> {
        let rows = self
            .completed
            .records
            .iter()
            .zip(&self.references)
            .map(|(row, reference)| {
                let TelemetryRecord::Activity(observation) = &row.record else {
                    return Err(invalid());
                };
                Ok(ActivityFrameRow { observation, reference: Some(reference) })
            })
            .collect::<Result<Vec<_>>>()?;
        ActivityFrameEvidence::new(self.source(), &rows, true)
    }
}

/// Both inputs retain their own source, counts, native clocks and receipts. The outer bytes/deadline are cumulative.
#[derive(Debug)]
pub struct CompletedBlocking {
    original: CompletedActivity,
    followup: CompletedActivity,
    bytes_read: u64,
}
impl CompletedBlocking {
    fn new(original: CompletedActivity, followup: CompletedActivity, bytes_read: u64) -> Result<Self> {
        let SourceObservation::FocusedBlocking { result, .. } = &followup.source().observation else {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        };
        if original.capture() == followup.capture()
            || original.capture() != &result.focus.parent.capture_ref()
            || original.connection() != followup.connection()
        {
            return Err(invalid());
        }
        if !original.frame()?.has_session_generation_authority(&result.focus.original.key.session) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let row = original
            .records()
            .iter()
            .zip(original.references())
            .find(|(_, reference)| **reference == result.focus.parent)
            .map(|(row, _)| row)
            .ok_or_else(invalid)?;
        let TelemetryRecord::Activity(observation) = &row.record else {
            return Err(invalid());
        };
        if !result.focus.matches_parent(original.source(), observation, &result.focus.parent)
            || !result.accepts_frame(&followup.frame()?)
        {
            return Err(invalid());
        }
        Ok(Self { original, followup, bytes_read })
    }
    pub fn original(&self) -> &CompletedActivity {
        &self.original
    }
    pub fn followup(&self) -> &CompletedActivity {
        &self.followup
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    pub fn into_inputs(self) -> (CompletedActivity, CompletedActivity) {
        (self.original, self.followup)
    }
}

impl OutboxDelivery {
    /// One Activity capture uses the same canonical worker, journal binding and final cancellation check.
    pub async fn with_completed_activity<T: Send + 'static>(
        &self,
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(CompletedActivity) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let read = CompletedCaptureRead::activity_lookup(lookup, budget, operation)?;
        self.with_capture_worker(read, worker_guard, move |files, read| {
            project(CompletedActivity::from_completed(files.completed_capture(read)?)?)
        })
        .await
    }

    /// No new worker or verifier: both exact captures share the existing lock, cancellation and cumulative budget.
    pub async fn with_completed_blocking<T: Send + 'static>(
        &self,
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(CompletedBlocking) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let read = CompletedCaptureRead::activity_lookup(lookup, budget, operation)?;
        let connection = lookup.connection.to_owned();
        self.with_capture_worker(read, worker_guard, move |files, read| {
            let followup = CompletedActivity::from_completed(files.completed_capture(read)?)?;
            let SourceObservation::FocusedBlocking { result, .. } = &followup.source().observation else {
                return Err(DeliveryProblem::UnsupportedSchema.error());
            };
            read.select_activity_reference(&connection, result.focus.parent.capture_ref())?;
            let original = CompletedActivity::from_completed(files.completed_capture(read)?)?;
            project(CompletedBlocking::new(original, followup, read.bytes_read)?)
        })
        .await
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests;
