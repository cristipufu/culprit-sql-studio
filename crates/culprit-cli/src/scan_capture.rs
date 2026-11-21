//! Explicit local originals enter the existing scan owner; ordinary scans never open an Outbox.
use clap::Args;
use culprit_collector::{capture::CollectionCancellation, outbox::CompletedCaptureBudget};
use culprit_core::{
    Report, Result,
    limits::OperationGuard,
    telemetry::{CollectionStop, DeliveryProblem},
};
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, OutboxCaptureReader,
    Workspace,
};
use std::{future::Future, path::PathBuf, pin::Pin};

type ScanTask<'a> = Pin<Box<dyn Future<Output = Result<Box<Report>>> + 'a>>;

#[derive(Args, Default)]
#[command(group(clap::ArgGroup::new("captured_original")
    .args(["memory_clerks_capture", "hyperscale_log_components_capture", "tempdb_metadata_memory_capture", "pool_settings_capture", "managed_instance_resources_capture", "cdc_capture", "change_feed_capture", "numa_nodes_capture"]).multiple(false)))]
pub(crate) struct CapturedScanArgs {
    #[arg(long,value_name="OBSERVATION",requires="evidence_outbox",help=culprit_core::t!("view.numa_nodes.intake"))]
    numa_nodes_capture: Option<String>,
    #[arg(long,value_name="OBSERVATION",requires="evidence_outbox",help=culprit_core::t!("view.change_capture.history.intake"))]
    cdc_capture: Option<String>,
    #[arg(long,value_name="OBSERVATION",requires="evidence_outbox",help=culprit_core::t!("view.change_capture.history.intake"))]
    change_feed_capture: Option<String>,
    #[arg(long, value_name = "DIRECTORY", requires = "captured_original", help = culprit_core::t!("view.hyperscale_log_components.intake.outbox"))]
    evidence_outbox: Option<PathBuf>,
    #[arg(long, value_name = "OBSERVATION", requires = "evidence_outbox", help = culprit_core::t!("cli.scan.captured_clerks.observation"))]
    memory_clerks_capture: Option<String>,
    #[arg(long, value_name = "OBSERVATION", requires = "evidence_outbox", help = culprit_core::t!("view.hyperscale_log_components.intake.observation"))]
    hyperscale_log_components_capture: Option<String>,
    #[arg(long, value_name = "OBSERVATION", requires = "evidence_outbox", help = culprit_core::t!("view.tempdb_metadata_memory.intake.observation"))]
    tempdb_metadata_memory_capture: Option<String>,
    #[arg(long,value_name="OBSERVATION",requires="evidence_outbox",help=culprit_core::t!("view.pool_settings.intake.observation"))]
    pool_settings_capture: Option<String>,
    #[arg(long,value_name="OBSERVATION",requires="evidence_outbox",help=culprit_core::t!("view.mi_resources.intake"))]
    managed_instance_resources_capture: Option<String>,
}

#[derive(Clone, Copy)]
struct CapturedOriginal<'a> {
    observation_id: &'a str,
    analysis: LocalCaptureAnalysisKind,
}
impl CapturedScanArgs {
    fn original(&self) -> Result<Option<CapturedOriginal<'_>>> {
        let mut selected = [
            (self.numa_nodes_capture.as_deref(), LocalCaptureAnalysisKind::InstanceResources),
            (self.cdc_capture.as_deref(), LocalCaptureAnalysisKind::Cdc),
            (self.change_feed_capture.as_deref(), LocalCaptureAnalysisKind::ChangeFeed),
            (self.memory_clerks_capture.as_deref(), LocalCaptureAnalysisKind::MemoryClerks),
            (self.hyperscale_log_components_capture.as_deref(), LocalCaptureAnalysisKind::HyperscaleLogComponents),
            (self.tempdb_metadata_memory_capture.as_deref(), LocalCaptureAnalysisKind::TempdbMetadataMemory),
            (self.pool_settings_capture.as_deref(), LocalCaptureAnalysisKind::PoolSettings),
            (self.managed_instance_resources_capture.as_deref(), LocalCaptureAnalysisKind::ManagedInstanceResources),
        ]
        .into_iter()
        .filter_map(|(observation_id, analysis)| {
            observation_id.map(|observation_id| CapturedOriginal { observation_id, analysis })
        });
        let original = selected.next();
        if selected.next().is_some() {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Ok(original)
    }
}

pub(crate) fn scan<'a>(workspace: &'a Workspace, args: &'a crate::ScanArgs) -> ScanTask<'a> {
    Box::pin(scan_body(workspace, args))
}
async fn scan_body(workspace: &Workspace, args: &crate::ScanArgs) -> Result<Box<Report>> {
    let options = args.snapshot_options();
    match (&args.captured.evidence_outbox, args.captured.original()?) {
        (None, None) => {
            boxed_report(workspace.scan_with_log(&args.connection, &options, args.query_log.as_deref())).await
        }
        (Some(directory), Some(original)) => {
            let reader = OutboxCaptureReader::directory(directory.clone(), CompletedCaptureBudget::default())?;
            let cancellation = CollectionCancellation::default();
            let action = captured_scan(workspace, args, &reader, original, &cancellation);
            crate::collection_shutdown::run(&cancellation, action).await
        }
        _ => Err(DeliveryProblem::InvalidContract.error()),
    }
}

fn captured_scan<'a>(
    workspace: &'a Workspace,
    args: &'a crate::ScanArgs,
    reader: &'a dyn LocalCaptureReader,
    original: CapturedOriginal<'a>,
    cancellation: &'a CollectionCancellation,
) -> ScanTask<'a> {
    Box::pin(captured_scan_body(workspace, args, reader, original, cancellation))
}
async fn captured_scan_body(
    workspace: &Workspace,
    args: &crate::ScanArgs,
    reader: &dyn LocalCaptureReader,
    original: CapturedOriginal<'_>,
    cancellation: &CollectionCancellation,
) -> Result<Box<Report>> {
    let captured = read(workspace, reader, &args.connection, original, cancellation).await?;
    let options = args.snapshot_options();
    // The owning cancellation check precedes construction, just as for every existing scan action.
    scan_until_cancelled(cancellation, || captured_action(workspace, args, &options, captured)).await
}

/// Select one future without an all-arm async frame or inline completed reports. No task is detached.
fn captured_action<'a>(
    workspace: &'a Workspace,
    args: &'a crate::ScanArgs,
    options: &'a culprit_core::SnapshotOptions,
    captured: LocalCapturedAnalysis,
) -> ScanTask<'a> {
    match captured {
        LocalCapturedAnalysis::ManagedInstanceResources(captured) => {
            boxed_report(workspace.scan_with_captured_managed_instance_resources(
                &args.connection,
                options,
                args.query_log.as_deref(),
                *captured,
            ))
        }
        LocalCapturedAnalysis::MemoryClerks(captured) => boxed_report(workspace.scan_with_captured_memory_clerks(
            &args.connection,
            options,
            args.query_log.as_deref(),
            captured,
        )),
        LocalCapturedAnalysis::HyperscaleLogComponents(captured) => {
            boxed_report(workspace.scan_with_captured_hyperscale_log_components(
                &args.connection,
                options,
                args.query_log.as_deref(),
                captured,
            ))
        }
        LocalCapturedAnalysis::PoolSettings(captured) => boxed_report(workspace.scan_with_captured_pool_settings(
            &args.connection,
            options,
            args.query_log.as_deref(),
            *captured,
        )),
        LocalCapturedAnalysis::Snapshot(captured)
            if captured.report().kind() == LocalCaptureAnalysisKind::InstanceResources =>
        {
            boxed_report(workspace.scan_with_captured_numa_nodes(
                &args.connection,
                options,
                args.query_log.as_deref(),
                captured,
            ))
        }
        LocalCapturedAnalysis::Snapshot(captured)
            if matches!(
                captured.report().kind(),
                LocalCaptureAnalysisKind::Cdc | LocalCaptureAnalysisKind::ChangeFeed
            ) =>
        {
            boxed_report(workspace.scan_with_captured_change_history(
                &args.connection,
                options,
                args.query_log.as_deref(),
                captured,
            ))
        }
        LocalCapturedAnalysis::Snapshot(captured) => boxed_report(workspace.scan_with_captured_tempdb_metadata_memory(
            &args.connection,
            options,
            args.query_log.as_deref(),
            captured,
        )),
        LocalCapturedAnalysis::Replication(_)
        | LocalCapturedAnalysis::PageServerReads(_)
        | LocalCapturedAnalysis::QueryWaitFragments(_)
        | LocalCapturedAnalysis::RuntimeFragments(_)
        | LocalCapturedAnalysis::Blocking(_)
        | LocalCapturedAnalysis::SessionCensus(_) => {
            Box::pin(std::future::ready(Err(DeliveryProblem::InvalidContract.error())))
        }
    }
}
/// The selected public scan may return a large Report; only this adapter transports it into CLI ownership.
fn boxed_report<'a>(work: impl Future<Output = Result<Report>> + 'a) -> ScanTask<'a> {
    Box::pin(async move { work.await.map(Box::new) })
}

async fn scan_until_cancelled<T, F>(cancellation: &CollectionCancellation, start: impl FnOnce() -> F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    ensure_not_cancelled(cancellation)?;
    let scanning = start();
    tokio::pin!(scanning);
    let report = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            // The normal scan API has no cancel-and-await settlement. Retire this same future so its
            // native guard retains any unresolved debt; never claim confirmed native shutdown.
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        result = &mut scanning => result,
    }?;
    ensure_not_cancelled(cancellation)?;
    Ok(report)
}

fn ensure_not_cancelled(cancellation: &CollectionCancellation) -> Result<()> {
    if cancellation.is_cancelled() { Err(CollectionStop::Cancelled.error()) } else { Ok(()) }
}

async fn read(
    workspace: &Workspace,
    reader: &dyn LocalCaptureReader,
    connection: &str,
    original: CapturedOriginal<'_>,
    cancellation: &CollectionCancellation,
) -> Result<LocalCapturedAnalysis> {
    let guard = OperationGuard::new(reader.timeout())?;
    let reading = workspace.analyze_local_capture(
        reader,
        LocalCaptureRequest {
            connection,
            observation_id: original.observation_id,
            analysis: original.analysis,
            operation: guard.handle(),
        },
    );
    tokio::pin!(reading);
    let result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            drop(guard);
            // Retain the original local read until its started worker releases ownership.
            reading.await
        }
        result = &mut reading => result,
    }?;
    ensure_not_cancelled(cancellation)?;
    Ok(result)
}

#[cfg(test)]
mod tests;
