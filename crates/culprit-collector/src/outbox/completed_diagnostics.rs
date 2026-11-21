//! A thin selection on the existing canonical worker, never a second journal/decoder/history store.
use super::{
    OutboxDelivery,
    completed_capture::{CompletedCapture, CompletedCaptureBudget, CompletedCaptureLookup, CompletedCaptureRead},
};
use culprit_core::{Result, limits::OperationHandle, telemetry::*};

#[derive(Debug)]
pub struct CompletedDiagnostics {
    completed: CompletedCapture,
}
impl CompletedDiagnostics {
    pub fn connection(&self) -> &str {
        &self.completed.connection
    }
    pub fn source(&self) -> &CollectionDescription {
        &self.completed.source
    }
    pub fn original(&self) -> &DiagnosticCaptureRef {
        &self.completed.capture
    }
    pub fn records(&self) -> &[ExportedRecord] {
        &self.completed.records
    }
    pub fn row_ordinals(&self) -> &[u32] {
        &self.completed.row_ordinals
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
}
impl OutboxDelivery {
    pub async fn with_completed_diagnostics<T: Send + 'static>(
        &self,
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(CompletedDiagnostics) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let read = CompletedCaptureRead::diagnostics_lookup(lookup, budget, operation)?;
        self.with_completed_capture(read, worker_guard, move |completed| project(CompletedDiagnostics { completed }))
            .await
    }
}
