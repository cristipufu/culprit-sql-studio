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
use std::{future::Future, path::PathBuf};

#[derive(Args, Default)]
#[command(group(clap::ArgGroup::new("captured_original")
    .args(["memory_clerks_capture", "hyperscale_log_components_capture", "tempdb_metadata_memory_capture"]).multiple(false)))]
pub(crate) struct CapturedScanArgs {
    #[arg(long, value_name = "DIRECTORY", requires = "captured_original", help = culprit_core::t!("view.hyperscale_log_components.intake.outbox"))]
    evidence_outbox: Option<PathBuf>,
    #[arg(long, value_name = "OBSERVATION", requires = "evidence_outbox", help = culprit_core::t!("cli.scan.captured_clerks.observation"))]
    memory_clerks_capture: Option<String>,
    #[arg(long, value_name = "OBSERVATION", requires = "evidence_outbox", help = culprit_core::t!("view.hyperscale_log_components.intake.observation"))]
    hyperscale_log_components_capture: Option<String>,
    #[arg(long, value_name = "OBSERVATION", requires = "evidence_outbox", help = culprit_core::t!("view.tempdb_metadata_memory.intake.observation"))]
    tempdb_metadata_memory_capture: Option<String>,
}

#[derive(Clone, Copy)]
struct CapturedOriginal<'a> {
    observation_id: &'a str,
    analysis: LocalCaptureAnalysisKind,
}
impl CapturedScanArgs {
    fn original(&self) -> Result<Option<CapturedOriginal<'_>>> {
        let original = match (
            &self.memory_clerks_capture,
            &self.hyperscale_log_components_capture,
            &self.tempdb_metadata_memory_capture,
        ) {
            (None, None, None) => return Ok(None),
            (Some(observation_id), None, None) => {
                CapturedOriginal { observation_id, analysis: LocalCaptureAnalysisKind::MemoryClerks }
            }
            (None, Some(observation_id), None) => {
                CapturedOriginal { observation_id, analysis: LocalCaptureAnalysisKind::HyperscaleLogComponents }
            }
            (None, None, Some(observation_id)) => {
                CapturedOriginal { observation_id, analysis: LocalCaptureAnalysisKind::TempdbMetadataMemory }
            }
            _ => return Err(DeliveryProblem::InvalidContract.error()),
        };
        Ok(Some(original))
    }
}

pub(crate) async fn scan(workspace: &Workspace, args: &crate::ScanArgs) -> Result<Report> {
    let options = args.snapshot_options();
    match (&args.captured.evidence_outbox, args.captured.original()?) {
        (None, None) => workspace.scan_with_log(&args.connection, &options, args.query_log.as_deref()).await,
        (Some(directory), Some(original)) => {
            let reader = OutboxCaptureReader::directory(directory.clone(), CompletedCaptureBudget::default())?;
            let cancellation = CollectionCancellation::default();
            let action = captured_scan(workspace, args, &reader, original, &cancellation);
            crate::collection_shutdown::run(&cancellation, action).await
        }
        _ => Err(DeliveryProblem::InvalidContract.error()),
    }
}

async fn captured_scan(
    workspace: &Workspace,
    args: &crate::ScanArgs,
    reader: &dyn LocalCaptureReader,
    original: CapturedOriginal<'_>,
    cancellation: &CollectionCancellation,
) -> Result<Report> {
    let captured = read(workspace, reader, &args.connection, original, cancellation).await?;
    let options = args.snapshot_options();
    match captured {
        LocalCapturedAnalysis::MemoryClerks(captured) => {
            scan_until_cancelled(cancellation, || {
                workspace.scan_with_captured_memory_clerks(
                    &args.connection,
                    &options,
                    args.query_log.as_deref(),
                    captured,
                )
            })
            .await
        }
        LocalCapturedAnalysis::HyperscaleLogComponents(captured) => {
            scan_until_cancelled(cancellation, || {
                workspace.scan_with_captured_hyperscale_log_components(
                    &args.connection,
                    &options,
                    args.query_log.as_deref(),
                    captured,
                )
            })
            .await
        }
        LocalCapturedAnalysis::Snapshot(captured) => {
            scan_until_cancelled(cancellation, || {
                workspace.scan_with_captured_tempdb_metadata_memory(
                    &args.connection,
                    &options,
                    args.query_log.as_deref(),
                    captured,
                )
            })
            .await
        }
        LocalCapturedAnalysis::PageServerReads(_)
        | LocalCapturedAnalysis::Blocking(_)
        | LocalCapturedAnalysis::SessionCensus(_) => Err(DeliveryProblem::InvalidContract.error()),
    }
}

async fn scan_until_cancelled<F>(cancellation: &CollectionCancellation, start: impl FnOnce() -> F) -> Result<Report>
where
    F: Future<Output = Result<Report>>,
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
