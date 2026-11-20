//! One opaque local read attaches only after the scan owner settles; no native tempdb query is added.
use super::*;
use culprit_collector::analytical::test_support::{TempdbMetadataMemoryCase, tempdb_metadata_memory_evidence};
use culprit_core::Report;
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureRequest, LocalCapturedAnalysis, LocalSnapshotAnalysis, OutboxCaptureReader,
};

async fn analyze(workspace: &Workspace, captured: &Captured, kind: LocalCaptureAnalysisKind) -> LocalSnapshotAnalysis {
    let reader = OutboxCaptureReader::delivery(captured.outbox.delivery(), CompletedCaptureBudget::default()).unwrap();
    let guard = culprit_core::limits::OperationGuard::new(std::time::Duration::from_secs(5)).unwrap();
    let result = workspace
        .analyze_local_capture(
            &reader,
            LocalCaptureRequest {
                connection: "shop",
                observation_id: &captured.manifest.capture.observation_id,
                analysis: kind,
                operation: guard.handle(),
            },
        )
        .await
        .unwrap();
    let LocalCapturedAnalysis::Snapshot(result) = result else { panic!("generic original snapshot owner") };
    result
}
fn assert_original(report: &Report, source: &CollectionDescription) {
    let read = report.snapshot.operations.tempdb_metadata_memory.as_deref().unwrap();
    assert!(read.is_valid() && read.read_complete);
    assert_eq!(&read.description, source);
    assert_eq!(read.captured_references().unwrap().len(), read.observations.len());
    let context = read.context().unwrap();
    assert!(matches!(&context.target, CollectionTarget::Shared(owner) if owner.kind==SharedResourceKind::Instance));
    assert!(matches!(&context.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }));
    let view = report.tempdb_metadata_memory.as_deref().unwrap();
    assert_eq!(view.context.native(), Some(&context));
    assert_eq!(view.native_rows, source.observation.snapshot().unwrap().sources[0].records);
    assert_eq!(view.observed_rows, view.native_rows);
    assert_eq!(view.feature_observed_at.as_deref(), Some("2020-01-01T12:00:00.2Z"));
    assert_ne!(context.observed_from, report.snapshot.taken_at);
    let retained = report.evidence.as_ref().unwrap().collected.as_ref().unwrap();
    assert_eq!(retained.operations.tempdb_metadata_memory.as_deref(), Some(read));
    assert_eq!(
        serde_json::to_value(culprit_engine::reports::display(report)).unwrap()["tempdb_metadata_memory"],
        serde_json::to_value(view).unwrap()
    );
}

#[tokio::test]
async fn original_metadata_scope_survives_unknown_current_identity_save_and_offline_reanalysis() {
    for case in TempdbMetadataMemoryCase::ALL {
        let (source, rows) = tempdb_metadata_memory_evidence(case);
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let mut batch = snapshots();
        batch.requested.identity = None;
        batch.requested.runtime_populations = None;
        batch.populations.clear();
        batch.catalog = None;
        let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
        let original = analyze(&workspace, &captured, LocalCaptureAnalysisKind::TempdbMetadataMemory).await;
        let expected = match original.report() {
            culprit_engine::LocalSnapshotReport::TempdbMetadataMemory(view) => view.clone(),
            _ => panic!(),
        };
        let report =
            workspace.scan_with_captured_tempdb_metadata_memory("shop", &options(), None, original).await.unwrap();
        assert_original(&report, &source);
        assert!(report.snapshot.identity.is_none());
        assert_eq!(report.tempdb_metadata_memory.as_deref(), Some(expected.as_ref()));
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
        let id = report.snapshot.id.clone();
        drop(captured);
        assert_original(&workspace.report_at("shop", &id).unwrap(), &source);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1, "saved replay opens no provider or local capture");
    }
}

#[tokio::test]
async fn wrong_connection_or_selected_report_kind_refuses_before_any_native_scan() {
    let (source, rows) = tempdb_metadata_memory_evidence(TempdbMetadataMemoryCase::Reserved);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let original = analyze(&workspace, &captured, LocalCaptureAnalysisKind::TempdbMetadataMemory).await;
    assert_eq!(
        workspace
            .scan_with_captured_tempdb_metadata_memory("another-connection", &options(), None, original)
            .await
            .unwrap_err()
            .detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract))
    );
    let (source, rows) = culprit_collector::analytical::test_support::performance_counter_evidence(
        culprit_collector::analytical::test_support::PerformanceCounterCase::Exact,
    );
    let wrong = Captured::from_source("shop", source, rows).await;
    let original = analyze(&workspace, &wrong, LocalCaptureAnalysisKind::PerformanceCounters).await;
    assert_eq!(
        workspace
            .scan_with_captured_tempdb_metadata_memory("shop", &options(), None, original)
            .await
            .unwrap_err()
            .detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract))
    );
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert!(workspace.report_history("shop").unwrap().is_empty());
}

#[tokio::test]
async fn conflicting_original_or_unconfirmed_scan_shutdown_cannot_publish_metadata_context() {
    for conflict in [true, false] {
        let (source, rows) = tempdb_metadata_memory_evidence(TempdbMetadataMemoryCase::Reserved);
        let captured = Captured::from_source("shop", source, rows).await;
        let mut batch = snapshots();
        if conflict {
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
            batch.requested.operations.tempdb_metadata_memory = Some(Box::new(read));
        }
        let (_home, workspace, state) =
            workspace(batch, if conflict { SourceShutdown::Quiescent } else { SourceShutdown::Unconfirmed });
        let original = analyze(&workspace, &captured, LocalCaptureAnalysisKind::TempdbMetadataMemory).await;
        let error =
            workspace.scan_with_captured_tempdb_metadata_memory("shop", &options(), None, original).await.unwrap_err();
        if conflict {
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Conflict)));
        } else {
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
        }
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert!(state.analyzed.lock().unwrap().is_empty());
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}

#[tokio::test]
async fn sharing_withholds_indivisible_original_receipts_and_preserves_a_repeatable_unavailable_family() {
    let (source, rows) = tempdb_metadata_memory_evidence(TempdbMetadataMemoryCase::Reserved);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, _) = workspace(snapshots(), SourceShutdown::Quiescent);
    let original = analyze(&workspace, &captured, LocalCaptureAnalysisKind::TempdbMetadataMemory).await;
    let report = workspace.scan_with_captured_tempdb_metadata_memory("shop", &options(), None, original).await.unwrap();
    let shared = culprit_core::redact::redact_snapshot(&report.snapshot);
    assert!(shared.operations.tempdb_metadata_memory.is_none());
    assert!(report.snapshot.operations.tempdb_metadata_memory.is_some());
    let source =
        shared.sources.iter().find(|source| source.source == culprit_core::source_id::TEMPDB_METADATA_MEMORY).unwrap();
    assert_eq!(source.state, SourceState::Unavailable);
    assert_eq!(source.reason_message, Some(culprit_core::message!("view.tempdb_metadata_memory.sharing_withheld")));
    assert!(culprit_checks::analyze(shared.clone()).tempdb_metadata_memory.is_none());
    let again = culprit_core::redact::redact_snapshot(&shared);
    assert_eq!(
        again
            .sources
            .iter()
            .find(|source| source.source == culprit_core::source_id::TEMPDB_METADATA_MEMORY)
            .unwrap()
            .reason_message,
        source.reason_message
    );
    let text = serde_json::to_string(&shared).unwrap();
    for private in ["native-instance-a", "native-instance-namespace", "native_consumer_id", "CapturedOperationalRead"] {
        assert!(!text.contains(private), "{private}");
    }
}
