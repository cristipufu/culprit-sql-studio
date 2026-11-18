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
pub(crate) struct CapturedScanArgs {
    #[arg(long, value_name = "DIRECTORY", requires = "memory_clerks_capture", help = culprit_core::t!("cli.scan.captured_clerks.outbox"))]
    evidence_outbox: Option<PathBuf>,
    #[arg(long, value_name = "OBSERVATION", requires = "evidence_outbox", help = culprit_core::t!("cli.scan.captured_clerks.observation"))]
    memory_clerks_capture: Option<String>,
}

pub(crate) async fn scan(workspace: &Workspace, args: &crate::ScanArgs) -> Result<Report> {
    let options = args.snapshot_options();
    let CapturedScanArgs { evidence_outbox, memory_clerks_capture } = &args.captured;
    match (evidence_outbox, memory_clerks_capture) {
        (None, None) => workspace.scan_with_log(&args.connection, &options, args.query_log.as_deref()).await,
        (Some(directory), Some(observation_id)) => {
            let reader = OutboxCaptureReader::directory(directory.clone(), CompletedCaptureBudget::default())?;
            let cancellation = CollectionCancellation::default();
            let action = captured_scan(workspace, args, &reader, observation_id, &cancellation);
            crate::collection_shutdown::run(&cancellation, action).await
        }
        _ => Err(DeliveryProblem::InvalidContract.error()),
    }
}

async fn captured_scan(
    workspace: &Workspace,
    args: &crate::ScanArgs,
    reader: &dyn LocalCaptureReader,
    observation_id: &str,
    cancellation: &CollectionCancellation,
) -> Result<Report> {
    let captured = read(workspace, reader, &args.connection, observation_id, cancellation).await?;
    let options = args.snapshot_options();
    scan_until_cancelled(cancellation, || {
        workspace.scan_with_captured_memory_clerks(&args.connection, &options, args.query_log.as_deref(), captured)
    })
    .await
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
    observation_id: &str,
    cancellation: &CollectionCancellation,
) -> Result<culprit_engine::LocalMemoryClerks> {
    let guard = OperationGuard::new(reader.timeout())?;
    let reading = workspace.analyze_local_capture(
        reader,
        LocalCaptureRequest {
            connection,
            observation_id,
            analysis: LocalCaptureAnalysisKind::MemoryClerks,
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
    match result {
        LocalCapturedAnalysis::MemoryClerks(captured) => {
            ensure_not_cancelled(cancellation)?;
            Ok(captured)
        }
        LocalCapturedAnalysis::PageServerReads(_) => Err(DeliveryProblem::InvalidContract.error()),
    }
}

#[cfg(test)]
mod tests;
