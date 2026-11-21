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
mod blocking;
mod reports;

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
        match request.analysis.read_plan() {
            LocalCaptureReadPlan::Replication => {
                delivery
                    .with_completed_snapshot(
                        lookup,
                        SnapshotDataset::Replication,
                        self.budget,
                        request.operation,
                        permit,
                        move |capture| {
                            let result = explain_replication(capture)
                                .map(|read| LocalCapturedAnalysis::Replication(Box::new(read)));
                            #[cfg(test)]
                            if let Some(boundary) = boundary {
                                boundary()?;
                            }
                            result
                        },
                    )
                    .await
            }
            LocalCaptureReadPlan::QueryHistory => {
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
            LocalCaptureReadPlan::Snapshot(datasets) => {
                delivery
                    .with_completed_snapshots(
                        lookup,
                        datasets,
                        self.budget,
                        request.operation,
                        permit,
                        move |capture| {
                            let result = explain_operational(capture, request.analysis);
                            #[cfg(test)]
                            if let Some(boundary) = boundary {
                                boundary()?;
                            }
                            result
                        },
                    )
                    .await
            }
            LocalCaptureReadPlan::Activity => {
                delivery
                    .with_completed_activity(lookup, self.budget, request.operation, permit, move |capture| {
                        let original = blocking::local_read(capture)?;
                        let report = Box::new(culprit_checks::session_census::explain_frame(&original.frame()?)?);
                        #[cfg(test)]
                        if let Some(boundary) = boundary {
                            boundary()?;
                        }
                        Ok(LocalCapturedAnalysis::SessionCensus(Box::new(LocalSessionCensus { original, report })))
                    })
                    .await
            }
            LocalCaptureReadPlan::Blocking => {
                delivery
                    .with_completed_blocking(lookup, self.budget, request.operation, permit, move |capture| {
                        let result = blocking::explain(capture)
                            .map(|analysis| LocalCapturedAnalysis::Blocking(Box::new(analysis)));
                        #[cfg(test)]
                        if let Some(boundary) = boundary {
                            boundary()?;
                        }
                        result
                    })
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
            let TelemetryRecord::QueryDefinition(definition) = &row.row.record else {
                return Err(invalid());
            };
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
            read: &definition_read,
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

fn explain_replication(capture: CompletedSnapshot) -> Result<LocalReplication> {
    let source = capture.source().clone();
    let rows = capture
        .records()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let TelemetryRecord::Replication(point) = &row.record else { return Err(invalid()) };
            Ok((point.clone(), capture.reference(index)?))
        })
        .collect::<Result<Vec<_>>>()?;
    let report = crate::collected_resources::explain_replication_read(
        &source,
        rows.iter().map(|(row, _)| row.clone()).collect(),
        true,
    )?;
    Ok(LocalReplication {
        source,
        rows,
        report: Box::new(report),
        collected_from: capture.collected_from().into(),
        collected_until: capture.collected_until().into(),
        bytes_read: capture.bytes_read(),
    })
}

fn explain_operational(capture: CompletedSnapshot, kind: LocalCaptureAnalysisKind) -> Result<LocalCapturedAnalysis> {
    let connection = capture.connection().into();
    let collected_from = capture.collected_from().into();
    let collected_until = capture.collected_until().into();
    let bytes_read = capture.bytes_read();
    let original = LocalOperationalCapture {
        connection,
        read: Box::new(capture.into_operational_read()?),
        collected_from,
        collected_until,
        bytes_read,
    };
    match kind {
        LocalCaptureAnalysisKind::MemoryClerks => {
            let report = Box::new(culprit_checks::memory_clerks::explain_captured(original.read())?);
            Ok(LocalCapturedAnalysis::MemoryClerks(LocalMemoryClerks { original, report }))
        }
        LocalCaptureAnalysisKind::HyperscaleLogComponents => {
            let report = Box::new(culprit_checks::hyperscale_log_components::explain_captured(original.read())?);
            Ok(LocalCapturedAnalysis::HyperscaleLogComponents(LocalHyperscaleLogComponents { original, report }))
        }
        _ => {
            let report = reports::explain(kind, original.read())?;
            Ok(LocalCapturedAnalysis::Snapshot(LocalSnapshotAnalysis { original, report }))
        }
    }
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
