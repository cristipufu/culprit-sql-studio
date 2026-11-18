//! Only an explicitly installed Outbox owner can supply fresh local originals to the optional host capability.
use super::*;
use crate::collected_resources::{page_server_reads::original_rows, references::analysis_reference};
use culprit_checks::{
    page_server_reads::PageServerReadEvidence,
    query_store_definitions::{QueryStoreDefinitionEvidence, QueryStoreDefinitionRecord},
};
use culprit_collector::outbox::{
    CompletedCaptureBudget, CompletedCaptureLookup, CompletedQueryHistory, CompletedSnapshot, Outbox, OutboxDelivery,
};
use culprit_core::telemetry::*;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

enum LocalOutbox {
    Directory(PathBuf),
    Delivery(OutboxDelivery),
}

#[cfg(test)]
type ProjectionBoundary = Arc<dyn Fn() -> Result<()> + Send + Sync>;

/// Fixed startup-authorized local storage. No source configuration, credentials or warehouse adapter is opened.
pub struct OutboxCaptureReader {
    source: LocalOutbox,
    budget: CompletedCaptureBudget,
    readers: Arc<Semaphore>,
    #[cfg(test)]
    projection_boundary: Option<ProjectionBoundary>,
}

/// Last async ownership releases the directory lock before its share of the reader permit.
/// A started worker retains a second share until exclusive Files access has ended.
struct DirectoryOwnership {
    outbox: Outbox,
    permit: Arc<OwnedSemaphorePermit>,
}
impl OutboxCaptureReader {
    /// The directory is acquired only during an action. A separate active owner returns Locked; no read bypasses it.
    pub fn directory(directory: PathBuf, budget: CompletedCaptureBudget) -> Result<Self> {
        if directory.as_os_str().is_empty() {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Self::new(LocalOutbox::Directory(directory), budget)
    }

    /// A host that already owns collection can share the same serialized actor without a second filesystem owner.
    pub fn delivery(delivery: OutboxDelivery, budget: CompletedCaptureBudget) -> Result<Self> {
        Self::new(LocalOutbox::Delivery(delivery), budget)
    }

    fn new(source: LocalOutbox, budget: CompletedCaptureBudget) -> Result<Self> {
        Ok(Self {
            source,
            budget: budget.validate()?,
            readers: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            projection_boundary: None,
        })
    }

    async fn read(&self, request: LocalCaptureRequest<'_>) -> Result<LocalCapturedAnalysis> {
        check(&request.operation)?;
        let permit = tokio::time::timeout(request.operation.remaining(), self.readers.clone().acquire_owned())
            .await
            .map_err(|_| stopped(&request.operation))?
            .map_err(|_| DeliveryProblem::Storage.error())?;
        check(&request.operation)?;
        let permit = Arc::new(permit);
        match &self.source {
            LocalOutbox::Delivery(delivery) => self.project(delivery, request, permit.clone()).await,
            LocalOutbox::Directory(directory) => {
                let directory = directory.clone();
                let operation = request.operation.clone();
                let language = culprit_core::i18n::current();
                // The permit stays with started blocking work, including if its RPC caller is dropped.
                let (outbox, permit) = tokio::task::spawn_blocking(move || {
                    culprit_core::i18n::scope(&language.code, || {
                        check(&operation)?;
                        let outbox = Outbox::open(&directory)?;
                        check(&operation)?;
                        Ok::<_, culprit_core::Error>((outbox, permit))
                    })
                })
                .await
                .map_err(|_| DeliveryProblem::Storage.error())??;
                let ownership = DirectoryOwnership { outbox, permit };
                let delivery = ownership.outbox.delivery();
                let result = self.project(&delivery, request, ownership.permit.clone()).await;
                drop(delivery);
                drop(ownership);
                result
            }
        }
    }

    async fn project(
        &self,
        delivery: &OutboxDelivery,
        request: LocalCaptureRequest<'_>,
        permit: Arc<OwnedSemaphorePermit>,
    ) -> Result<LocalCapturedAnalysis> {
        #[cfg(test)]
        let boundary = self.projection_boundary.clone();
        let lookup = CompletedCaptureLookup { connection: request.connection, observation_id: request.observation_id };
        match request.analysis {
            LocalCaptureAnalysisKind::PageServerReads => {
                delivery
                    .with_completed_query_history(lookup, self.budget, request.operation, permit, move |capture| {
                        let result = explain_page_server_reads(capture).map(LocalCapturedAnalysis::PageServerReads);
                        #[cfg(test)]
                        if let Some(boundary) = boundary {
                            boundary()?;
                        }
                        result
                    })
                    .await
            }
            LocalCaptureAnalysisKind::HyperscaleLogComponents => {
                delivery
                    .with_completed_snapshot(
                        lookup,
                        SnapshotDataset::HyperscaleLogComponents,
                        self.budget,
                        request.operation,
                        permit,
                        move |capture| {
                            let result = explain_hyperscale_log_components(capture)
                                .map(LocalCapturedAnalysis::HyperscaleLogComponents);
                            #[cfg(test)]
                            if let Some(boundary) = boundary {
                                boundary()?;
                            }
                            result
                        },
                    )
                    .await
            }
            LocalCaptureAnalysisKind::MemoryClerks => {
                delivery
                    .with_completed_snapshot(
                        lookup,
                        SnapshotDataset::Operational,
                        self.budget,
                        request.operation,
                        permit,
                        move |capture| {
                            let result = explain_memory_clerks(capture).map(LocalCapturedAnalysis::MemoryClerks);
                            #[cfg(test)]
                            if let Some(boundary) = boundary {
                                boundary()?;
                            }
                            result
                        },
                    )
                    .await
            }
        }
    }
}
impl LocalCaptureReader for OutboxCaptureReader {
    fn timeout(&self) -> Duration {
        self.budget.timeout
    }
    fn analyze<'a>(&'a self, request: LocalCaptureRequest<'a>) -> LocalCaptureTask<'a> {
        Box::pin(self.read(request))
    }
}

fn explain_page_server_reads(capture: CompletedQueryHistory) -> Result<LocalPageServerReads> {
    let source = capture.source().clone();
    let original = capture.capture().clone();
    let history = source.query_history().ok_or_else(invalid)?;
    let history_from = history.interval.from.clone();
    let history_until = history.interval.until.clone();
    let population_membership = history.population_catalog.as_ref().map(|catalog| catalog.membership.clone());
    let selected_rows = capture.captured_rows(TelemetryStream::QueryRuntime).ok_or_else(invalid)?;
    let selected_definitions = capture.captured_rows(TelemetryStream::QueryDefinitions).ok_or_else(invalid)?;
    let reference_coverage = capture.reference_coverage().clone();
    let collected_from = capture.collected_from().into();
    let collected_until = capture.collected_until().into();
    let bytes_read = capture.bytes_read();
    let records = capture.into_analysis_records()?;
    let read = CapturedReadRef::new(
        &source,
        original.capture.clone(),
        original.manifest_sha256.clone(),
        vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows: selected_rows }],
    )?;
    let rows = original_rows(
        &source,
        &read,
        records.iter().filter(|row| row.row.record.stream() == TelemetryStream::QueryRuntime),
    )?;
    let definition_read = CapturedReadRef::new(
        &source,
        original.capture,
        original.manifest_sha256,
        vec![CapturedStreamRows { stream: TelemetryStream::QueryDefinitions, rows: selected_definitions }],
    )?;
    let binding = definition_read.bind(&source)?;
    let definitions = records
        .iter()
        .filter(|row| row.row.record.stream() == TelemetryStream::QueryDefinitions)
        .map(|row| {
            let TelemetryRecord::QueryDefinition(definition) = &row.row.record else { return Err(invalid()) };
            let CapturedRecordReference::Diagnostic { reference } = analysis_reference(&binding, row)? else {
                return Err(invalid());
            };
            Ok(QueryStoreDefinitionRecord { reference: *reference, definition })
        })
        .collect::<Result<Vec<_>>>()?;
    let report = culprit_checks::page_server_reads::explain_archive(PageServerReadEvidence {
        source: &source,
        read: &read,
        reference_coverage: &reference_coverage,
        rows: &rows,
        selected_rows,
        read_complete: true,
        definitions: Some(QueryStoreDefinitionEvidence {
            source: &source,
            rows: &definitions,
            selected_rows: selected_definitions,
            read_complete: true,
        }),
    })?;
    Ok(LocalPageServerReads {
        report: Box::new(report),
        history_from,
        history_until,
        population_membership,
        collected_from,
        collected_until,
        bytes_read,
    })
}

fn explain_hyperscale_log_components(capture: CompletedSnapshot) -> Result<LocalHyperscaleLogComponents> {
    let connection = capture.connection().into();
    let collected_from = capture.collected_from().into();
    let collected_until = capture.collected_until().into();
    let bytes_read = capture.bytes_read();
    let read = capture.into_operational_read()?;
    let report = culprit_checks::hyperscale_log_components::explain_captured(&read)?;
    Ok(LocalHyperscaleLogComponents {
        connection,
        read: Box::new(read),
        report: Box::new(report),
        collected_from,
        collected_until,
        bytes_read,
    })
}
fn explain_memory_clerks(capture: CompletedSnapshot) -> Result<LocalMemoryClerks> {
    let connection = capture.connection().into();
    let collected_from = capture.collected_from().into();
    let collected_until = capture.collected_until().into();
    let bytes_read = capture.bytes_read();
    let read = capture.into_operational_read()?;
    let report = culprit_checks::memory_clerks::explain_captured(&read)?;
    Ok(LocalMemoryClerks {
        connection,
        read: Box::new(read),
        report: Box::new(report),
        collected_from,
        collected_until,
        bytes_read,
    })
}

fn check(operation: &OperationHandle) -> Result<()> {
    if operation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    if operation.remaining().is_zero() {
        return Err(CollectionStop::TimedOut.error());
    }
    Ok(())
}
fn stopped(operation: &OperationHandle) -> culprit_core::Error {
    if operation.is_cancelled() { CollectionStop::Cancelled.error() } else { CollectionStop::TimedOut.error() }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests;
