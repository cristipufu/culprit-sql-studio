//! The actual shared Operational acquisition shape reaches a normal saved report through the existing owners.
use super::*;
use culprit_collector::analytical::test_support::{MemoryClerkCase, memory_clerk_evidence};
use culprit_core::Report;
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureRequest, LocalCapturedAnalysis, LocalMemoryClerks, OutboxCaptureReader,
};

async fn capture(partial: bool) -> Captured {
    let (mut source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    if partial {
        let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
        header.sources[0] = StateSourceCoverage::observed(culprit_core::source_id::MEMORY_CLERKS.into(), 2, false);
    }
    Captured::from_source("shop", source, rows).await
}
fn input<'a>(
    captured: &'a Captured,
    delivery: &'a OutboxDelivery,
    cancellation: &'a CollectionCancellation,
) -> CompletedCaptureScanInput<'a> {
    CompletedCaptureScanInput {
        analysis: CompletedCaptureAnalysis::MemoryClerks,
        ..captured.input(delivery, cancellation)
    }
}
async fn analyze(workspace: &Workspace, captured: &Captured) -> LocalMemoryClerks {
    let reader = OutboxCaptureReader::delivery(captured.outbox.delivery(), CompletedCaptureBudget::default()).unwrap();
    let guard = culprit_core::limits::OperationGuard::new(std::time::Duration::from_secs(5)).unwrap();
    let result = workspace
        .analyze_local_capture(
            &reader,
            LocalCaptureRequest {
                connection: "shop",
                observation_id: &captured.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::MemoryClerks,
                operation: guard.handle(),
            },
        )
        .await
        .unwrap();
    let LocalCapturedAnalysis::MemoryClerks(result) = result else { panic!("clerk result") };
    result
}
fn assert_original(report: &Report, source: &CollectionDescription) {
    let read = report.snapshot.operations.memory_clerks.as_deref().unwrap();
    assert!(read.is_valid());
    assert_eq!(&read.description, source);
    assert!(read.read_complete);
    assert_eq!(read.observations.len(), 3, "unrelated original Operational rows survive attachment");
    assert_eq!(read.captured_references().unwrap().len(), 3);
    let context = read.context().unwrap();
    assert!(matches!(&context.target, CollectionTarget::Shared(member) if member.kind == SharedResourceKind::Instance));
    assert!(matches!(&context.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }));
    assert_eq!(context.observed_from, "2020-01-01T12:00:00.000000001Z");
    assert_ne!(context.observed_from, report.snapshot.taken_at);
    let view = report.memory_clerks.as_deref().unwrap();
    assert_eq!(view.native_rows, Some(2));
    assert_eq!(view.observed_rows, 2);
    assert_eq!(view.read_complete, Some(true));
    assert_eq!(view.context.as_ref().and_then(ObservationReadContext::native), Some(&context));
    assert!(view.entries.iter().all(|entry| entry.scope == culprit_core::MeasurementScope::Server));
    assert_eq!(
        view.entries[0].fields.iter().find(|field| field.key == "pages_kb").unwrap().value.as_deref(),
        Some("9007199254740993")
    );
    assert_eq!(
        view.entries[0].fields.iter().find(|field| field.key == "shared_memory_reserved_kb").unwrap().value,
        None
    );
    let evidence = report.evidence.as_ref().unwrap().collected.as_ref().unwrap();
    assert_eq!(evidence.operations.memory_clerks.as_deref(), Some(read));
    assert!(
        report.snapshot.pressure.as_ref().is_none_or(|pressure| {
            pressure.resource_counters.as_deref().is_none_or(|sets| !sets.iter().any(memory_clerk_candidate))
        }),
        "captured rows are not copied into the unqualified live snapshot path"
    );
}

#[tokio::test]
async fn shared_clerk_originals_attach_after_shutdown_save_and_reanalyze_without_local_files() {
    let captured = capture(false).await;
    let source = captured.source().clone();
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let reports = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace
            .scan_populations_with_completed_capture(
                "shop",
                &options(),
                input(&captured, &captured.outbox.delivery(), &CollectionCancellation::default()),
                operation,
            )
            .await
    })
    .await
    .unwrap();
    assert_original(&reports.requested, &source);
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
    assert!(
        reports
            .populations
            .iter()
            .all(|report| report.snapshot.operations.memory_clerks.is_none() && report.memory_clerks.is_none())
    );
    let id = reports.requested.snapshot.id.clone();
    drop(captured);
    let reopened = workspace.report_at("shop", &id).unwrap();
    assert_original(&reopened, &source);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1, "offline reread constructs no provider");
}

#[tokio::test]
async fn opaque_local_result_preserves_historical_shared_scope_when_current_database_identity_is_unknown() {
    let captured = capture(true).await;
    let mut batch = snapshots();
    batch.requested.identity = None;
    batch.requested.runtime_populations = None;
    batch.populations.clear();
    batch.catalog = None;
    let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
    let original = analyze(&workspace, &captured).await;
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_captured_memory_clerks("shop", &options(), None, original, operation).await
    })
    .await
    .unwrap();
    assert_original(&report, captured.source());
    assert_eq!(report.memory_clerks.as_ref().unwrap().source_state, Some(SourceState::Partial));
    assert!(report.snapshot.identity.is_none());
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn original_journal_connection_is_checked_before_a_scan_provider_is_constructed() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let captured = Captured::from_source("another-saved-connection", source, rows).await;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    assert!(
        crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| workspace
            .scan_with_completed_capture(
                "shop",
                &options(),
                input(&captured, &captured.outbox.delivery(), &CollectionCancellation::default(),),
                operation
            )
            .await)
        .await
        .is_err()
    );
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert!(workspace.report_history("shop").unwrap().is_empty());
}

#[tokio::test]
async fn an_opaque_result_cannot_be_reassigned_to_a_different_saved_connection() {
    let captured = capture(false).await;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let original = analyze(&workspace, &captured).await;
    let error = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_captured_memory_clerks("another-connection", &options(), None, original, operation).await
    })
    .await
    .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn existing_captured_or_live_clerk_evidence_is_not_overwritten_or_merged() {
    for existing in ["captured", "live"] {
        let captured = capture(false).await;
        let mut batch = snapshots();
        if existing == "captured" {
            let read = captured
                .outbox
                .delivery()
                .completed_snapshot(
                    &captured.attempt,
                    SnapshotDataset::Operational,
                    CompletedCaptureBudget::default(),
                    &CollectionCancellation::default(),
                )
                .await
                .unwrap()
                .into_operational_read()
                .unwrap();
            batch.requested.operations.memory_clerks = Some(Box::new(read));
        } else {
            let TelemetryRecord::Operational(row) = &captured.rows[0] else { panic!() };
            let OperationalFact::CounterSet(set) = &row.fact else { panic!() };
            batch
                .requested
                .pressure
                .get_or_insert_with(Default::default)
                .resource_counters
                .get_or_insert_with(Vec::new)
                .push(set.as_ref().clone());
        }
        let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
        let error = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_with_completed_capture(
                    "shop",
                    &options(),
                    input(&captured, &captured.outbox.delivery(), &CollectionCancellation::default()),
                    operation,
                )
                .await
        })
        .await
        .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Conflict)), "{existing}");
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert!(state.analyzed.lock().unwrap().is_empty());
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}

#[tokio::test]
async fn corrupt_cancelled_or_oversized_clerk_reads_refuse_before_native_work() {
    for refusal in ["body", "manifest", "cancelled", "bytes"] {
        let captured = capture(false).await;
        let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
        let cancellation = CollectionCancellation::default();
        let delivery = captured.outbox.delivery();
        let mut request = input(&captured, &delivery, &cancellation);
        let mut attempt = captured.attempt.clone();
        match refusal {
            "body" => std::fs::write(captured.body(), b"{}\n").unwrap(),
            "manifest" => {
                attempt.manifest_sha256 = Some(digest(b"foreign manifest"));
                request.attempt = &attempt;
            }
            "cancelled" => cancellation.cancel(),
            "bytes" => request.budget.max_bytes = 1,
            _ => unreachable!(),
        }
        assert!(
            crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| workspace
                .scan_with_completed_capture("shop", &options(), request, operation)
                .await)
            .await
            .is_err(),
            "{refusal}"
        );
        assert_eq!(state.factories.load(Ordering::SeqCst), 0, "{refusal}");
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}

#[tokio::test]
async fn unconfirmed_native_shutdown_cannot_publish_independent_clerk_input() {
    let captured = capture(false).await;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Unconfirmed);
    let error = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace
            .scan_with_completed_capture(
                "shop",
                &options(),
                input(&captured, &captured.outbox.delivery(), &CollectionCancellation::default()),
                operation,
            )
            .await
    })
    .await
    .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert!(state.analyzed.lock().unwrap().is_empty());
    assert!(workspace.report_history("shop").unwrap().is_empty());
}

struct WrongAnalysis(Mutex<Option<LocalCapturedAnalysis>>);
impl culprit_engine::LocalCaptureReader for WrongAnalysis {
    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(5)
    }
    fn analyze<'a>(&'a self, _: LocalCaptureRequest<'a>) -> culprit_engine::LocalCaptureTask<'a> {
        let result = self.0.lock().unwrap().take().unwrap();
        Box::pin(async move { Ok(result) })
    }
}

#[tokio::test]
async fn the_workspace_facade_rejects_a_different_family_returned_by_an_injected_reader() {
    let captured = capture(false).await;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let original = analyze(&workspace, &captured).await;
    let reader = WrongAnalysis(Mutex::new(Some(LocalCapturedAnalysis::MemoryClerks(original))));
    let guard = culprit_core::limits::OperationGuard::new(std::time::Duration::from_secs(5)).unwrap();
    let error = workspace
        .analyze_local_capture(
            &reader,
            LocalCaptureRequest {
                connection: "shop",
                observation_id: &captured.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::PageServerReads,
                operation: guard.handle(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
}
