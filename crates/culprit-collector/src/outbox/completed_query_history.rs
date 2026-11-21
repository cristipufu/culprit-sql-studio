//! An original local history read has neither warehouse publication authority nor a live database owner.
use super::{
    OutboxDelivery,
    completed_capture::{CompletedCapture, CompletedCaptureBudget, CompletedCaptureLookup, CompletedCaptureRead},
};
use culprit_core::{Result, limits::OperationHandle, telemetry::*};

/// Closed native-history selection; this changes retained analysis rows, never artifact authentication.
#[derive(Clone, Copy, Debug)]
pub enum QueryHistoryReadKind {
    Runtime,
    Waits,
}
impl QueryHistoryReadKind {
    pub(super) fn retains(self, stream: TelemetryStream) -> bool {
        match self {
            Self::Runtime => matches!(stream, TelemetryStream::QueryRuntime | TelemetryStream::QueryDefinitions),
            Self::Waits => stream == TelemetryStream::QueryWaits,
        }
    }
}

/// Canonical native-history originals. Only the serialized Outbox verifier may construct this value.
/// Every original artifact is verified once; retained definitions never upgrade unresolved runtime receipts.
#[derive(Debug)]
pub struct CompletedQueryHistory {
    completed: CompletedCapture,
}
impl CompletedQueryHistory {
    pub fn connection(&self) -> &str {
        &self.completed.connection
    }

    pub fn into_wait_read(self) -> Result<CapturedQueryWaitRead> {
        let original = self.completed;
        let selected = original
            .streams
            .iter()
            .find(|selected| selected.stream == TelemetryStream::QueryWaits)
            .cloned()
            .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
        let read = CapturedReadRef::new(
            &original.source,
            original.capture.capture,
            original.capture.manifest_sha256,
            vec![selected],
        )?;
        CapturedQueryWaitRead::from_exports(
            original.source,
            read,
            original.references,
            (original.collected_from, original.collected_until),
            original.records.into_iter().zip(original.row_ordinals),
            true,
        )
    }

    pub fn source(&self) -> &CollectionDescription {
        &self.completed.source
    }
    pub fn capture(&self) -> &DiagnosticCaptureRef {
        &self.completed.capture
    }
    pub fn records(&self) -> &[ExportedRecord] {
        &self.completed.records
    }
    pub fn row_ordinals(&self) -> &[u32] {
        &self.completed.row_ordinals
    }
    pub fn captured_rows(&self, stream: TelemetryStream) -> Option<u32> {
        self.completed.streams.iter().find(|selected| selected.stream == stream).map(|selected| selected.rows)
    }
    pub fn reference_coverage(&self) -> &ReferenceCoverage {
        &self.completed.references
    }
    /// Collector acquisition clocks, independent of the original native history interval.
    pub fn collected_from(&self) -> &str {
        &self.completed.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.completed.collected_until
    }
    pub fn bytes_read(&self) -> u64 {
        self.completed.bytes_read
    }
    /// Move the verified original bodies once into the shared offline record shape. Its storage ordinal is
    /// one based; native/ref positions remain zero based and are converted at the shared adapter boundary.
    pub fn into_analysis_records(self) -> Result<Vec<culprit_core::collected_analysis::CollectedAnalysisRecord>> {
        self.completed
            .records
            .into_iter()
            .zip(self.completed.row_ordinals)
            .map(|(row, ordinal)| {
                Ok(culprit_core::collected_analysis::CollectedAnalysisRecord {
                    batch_id: row.batch_id.clone(),
                    row_ordinal: ordinal.checked_add(1).ok_or_else(|| DeliveryProblem::Integrity.error())?,
                    native_payload_sha256: row.payload_sha256()?,
                    row,
                })
            })
            .collect()
    }
}

impl OutboxDelivery {
    /// Lookup, queue and verification share the caller's original operation deadline. The journal supplies the
    /// actual receipt; a guessed capture ID cannot change its connection, original manifest or native scope.
    /// The lifetime guard stays with that worker until exclusive Files access has ended.
    pub async fn with_completed_query_history<T: Send + 'static>(
        &self,
        lookup: CompletedCaptureLookup<'_>,
        selection: QueryHistoryReadKind,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(CompletedQueryHistory) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let read = CompletedCaptureRead::query_history(lookup, selection, budget, operation)?;
        self.with_completed_capture(read, worker_guard, move |completed| project(CompletedQueryHistory { completed }))
            .await
    }
}

#[cfg(test)]
mod tests;
