//! The existing canonical worker authenticates one selected resource-history original and its saved connection.
use super::{
    OutboxDelivery,
    completed_capture::{CompletedCapture, CompletedCaptureBudget, CompletedCaptureLookup, CompletedCaptureRead},
};
use crate::capture::CollectionCancellation;
use culprit_core::{Result, limits::OperationHandle, telemetry::*};
#[derive(Debug)]
pub struct CompletedManagedInstanceResources {
    completed: CompletedCapture,
}
impl CompletedManagedInstanceResources {
    pub fn connection(&self) -> &str {
        &self.completed.connection
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
    pub fn into_read(self) -> Result<CapturedManagedInstanceResourceRead> {
        if self.completed.records.len() != self.completed.row_ordinals.len() {
            return Err(DeliveryProblem::Integrity.error());
        }
        let positions = self
            .completed
            .records
            .iter()
            .zip(&self.completed.row_ordinals)
            .map(|(row, ordinal)| CapturedSnapshotPosition::from_export(row, *ordinal))
            .collect::<Result<Vec<_>>>()?;
        let observations = self
            .completed
            .records
            .into_iter()
            .map(|row| match row.record {
                TelemetryRecord::ResourceSample(value) => Ok(value),
                _ => Err(DeliveryProblem::UnsupportedSchema.error()),
            })
            .collect::<Result<Vec<_>>>()?;
        CapturedManagedInstanceResourceRead::captured(
            self.completed.source,
            self.completed.capture,
            positions,
            observations,
            true,
        )
    }
}
impl OutboxDelivery {
    pub async fn with_completed_managed_instance_resources<T: Send + 'static>(
        &self,
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(CompletedManagedInstanceResources) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let read = CompletedCaptureRead::managed_instance_resources(lookup, budget, operation)?;
        self.with_completed_capture(read, worker_guard, move |completed| {
            project(CompletedManagedInstanceResources { completed })
        })
        .await
    }
    pub async fn completed_managed_instance_resources_for_connection(
        &self,
        connection: &str,
        attempt: &DiagnosticAttemptReceipt,
        budget: CompletedCaptureBudget,
        cancellation: &CollectionCancellation,
    ) -> Result<CompletedManagedInstanceResources> {
        let read = CompletedCaptureRead::managed_instance_resources_attempt(
            connection,
            attempt.clone(),
            budget,
            cancellation.clone(),
        )?;
        self.with_completed_capture(read, (), |completed| Ok(CompletedManagedInstanceResources { completed })).await
    }
}
