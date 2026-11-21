//! Qualification reads the actual closed collection through the existing canonical file verifier, never a new decoder.
use super::{
    OutboxDelivery,
    completed_capture::{CompletedCapture, CompletedCaptureBudget, CompletedCaptureRead},
};
use crate::capture::CollectionCancellation;
use culprit_core::{Result, telemetry::*};

/// Private-construction authenticated originals for native qualification policy, regardless of source family.
/// No serialized receipt or caller-supplied source can construct this value or confer native cost authority.
#[derive(Debug)]
pub struct CompletedQualificationOriginal {
    original: CompletedCapture,
}
impl CompletedQualificationOriginal {
    pub fn connection(&self) -> &str {
        &self.original.connection
    }
    pub fn source(&self) -> &CollectionDescription {
        &self.original.source
    }
    pub fn capture(&self) -> &DiagnosticCaptureRef {
        &self.original.capture
    }
    pub fn records(&self) -> &[ExportedRecord] {
        &self.original.records
    }
    pub fn row_ordinals(&self) -> &[u32] {
        &self.original.row_ordinals
    }
    pub fn bytes_read(&self) -> u64 {
        self.original.bytes_read
    }
    pub fn reference(&self, index: usize) -> Result<DiagnosticEvidenceRef> {
        let row = self.original.records.get(index).ok_or_else(|| DeliveryProblem::Integrity.error())?;
        let ordinal = *self.original.row_ordinals.get(index).ok_or_else(|| DeliveryProblem::Integrity.error())?;
        DiagnosticEvidenceRef::from_export(&self.original.source, &self.original.capture.manifest_sha256, row, ordinal)
    }
}
impl OutboxDelivery {
    pub async fn completed_qualification_for_connection(
        &self,
        connection: &str,
        attempt: &DiagnosticAttemptReceipt,
        collection: TelemetryCollection,
        budget: CompletedCaptureBudget,
        cancellation: &CollectionCancellation,
    ) -> Result<CompletedQualificationOriginal> {
        let read =
            CompletedCaptureRead::qualification(connection, attempt.clone(), collection, budget, cancellation.clone())?;
        self.with_completed_capture(read, (), |original| Ok(CompletedQualificationOriginal { original })).await
    }
}
