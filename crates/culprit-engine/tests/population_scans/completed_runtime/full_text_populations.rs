use super::*;
use culprit_collector::analytical::test_support::{FullTextPopulationCase, full_text_population_evidence};
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureRequest, LocalCapturedAnalysis, LocalSnapshotAnalysis, OutboxCaptureReader,
};
async fn analyze_original(workspace: &Workspace, captured: &Captured) -> LocalSnapshotAnalysis {
    let reader = OutboxCaptureReader::delivery(captured.outbox.delivery(), CompletedCaptureBudget::default()).unwrap();
    let guard = culprit_core::limits::OperationGuard::new(std::time::Duration::from_secs(5)).unwrap();
    let result = workspace
        .analyze_local_capture(
            &reader,
            LocalCaptureRequest {
                connection: "shop",
                observation_id: &captured.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::FullTextPopulations,
                operation: guard.handle(),
            },
        )
        .await
        .unwrap();
    let LocalCapturedAnalysis::Snapshot(result) = result else { panic!() };
    result
}
#[tokio::test]
async fn completed_full_text_original_attaches_only_after_scan_shutdown_and_replays_without_original_files() {
    for case in FullTextPopulationCase::ALL {
        let (source, rows) = full_text_population_evidence(case);
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
        let result = workspace
            .scan_populations_with_completed_capture(
                "shop",
                &options(),
                CompletedCaptureScanInput {
                    analysis: CompletedCaptureAnalysis::FullTextPopulations,
                    ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
                },
            )
            .await
            .unwrap();
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        let [RecordedOperationalRead::Captured { read }] =
            result.requested.snapshot.operations.full_text_populations.as_slice()
        else {
            panic!()
        };
        assert_eq!(read.description, source);
        assert!(read.is_valid());
        let reports = result.requested.full_text_populations.as_ref().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].original.as_ref().unwrap().context, read.context().unwrap());
        assert_eq!(reports[0].source.state, source.observation.snapshot().unwrap().sources[0].state);
        assert_eq!(result.populations.len(), 2);
        assert!(result.populations.iter().all(|report| report.snapshot.operations.full_text_populations.is_empty()
            && report.full_text_populations.is_none()));
        let expected = serde_json::to_value(&result.requested.full_text_populations).unwrap();
        let id = result.requested.snapshot.id.clone();
        drop(captured);
        let replay = workspace.report_at("shop", &id).unwrap();
        assert_eq!(serde_json::to_value(&replay.full_text_populations).unwrap(), expected);
        assert_eq!(
            serde_json::to_value(culprit_engine::reports::display(&replay)).unwrap()["full_text_populations"],
            expected
        );
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn imported_exact_repeats_and_late_same_identity_conflicts_follow_the_single_replay_policy() {
    let (source, rows) = full_text_population_evidence(FullTextPopulationCase::Processing);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, _) = workspace(snapshots(), SourceShutdown::Quiescent);
    let analysis = analyze_original(&workspace, &captured).await;
    let first = analysis.original().read().clone();
    let original = RecordedOperationalRead::Captured { read: Box::new(first.clone()) };
    let mut snapshot = snapshots().requested;
    snapshot.operations.full_text_populations = vec![original.clone(), original.clone()];
    let restored = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let replay = culprit_checks::analyze(restored);
    assert_eq!(replay.full_text_populations.as_ref().unwrap().len(), 1);
    assert_eq!(replay.snapshot.operations.full_text_populations.len(), 2, "raw repeated originals remain untouched");
    let mut contradictory = first;
    contradictory.origin.capture.manifest_sha256 = digest(b"different-original-manifest");
    assert!(contradictory.is_valid(), "untrusted saved structural validity is not collection consistency");
    let marker = RecordedOperationalRead::Unavailable {
        reason: culprit_core::message!("view.full_text_populations.sharing_withheld"),
    };
    snapshot.operations.full_text_populations = vec![original];
    snapshot.operations.full_text_populations.extend(std::iter::repeat_n(marker, MAX_RECORDED_OPERATIONAL_READS + 1));
    snapshot.operations.full_text_populations.push(RecordedOperationalRead::Captured { read: Box::new(contradictory) });
    let before = serde_json::to_value(&snapshot.operations.full_text_populations).unwrap();
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap());
    let reports = replay.full_text_populations.as_ref().unwrap();
    assert_eq!(reports.len(), MAX_RECORDED_OPERATIONAL_READS + 1);
    assert!(
        reports
            .iter()
            .all(|report| report.original.is_none() && report.native_rows.is_none() && report.entries.is_empty())
    );
    assert_eq!(serde_json::to_value(&replay.snapshot.operations.full_text_populations).unwrap(), before);
}
#[tokio::test]
async fn sharing_and_wrong_connection_keep_explicit_original_withdrawal_without_native_work() {
    let (source, rows) = full_text_population_evidence(FullTextPopulationCase::Processing);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let original = analyze_original(&workspace, &captured).await;
    assert!(
        workspace
            .scan_with_captured_full_text_populations("other-connection", &options(), None, original)
            .await
            .is_err()
    );
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    let original = analyze_original(&workspace, &captured).await;
    let report = workspace.scan_with_captured_full_text_populations("shop", &options(), None, original).await.unwrap();
    let shared = culprit_core::redact::redact_snapshot(&report.snapshot);
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&shared).unwrap()).unwrap());
    let [report] = replay.full_text_populations.as_deref().unwrap() else { panic!() };
    assert!(report.original.is_none());
    assert!(report.native_rows.is_none());
    assert!(report.entries.is_empty());
    assert_eq!(report.source.state, SourceState::Unavailable);
    let raw = serde_json::to_string(&replay.snapshot.operations.full_text_populations).unwrap();
    for private in ["native-machine", "native-member", "original-process-and-db", "manifest_sha256", "original.invalid"]
    {
        assert!(!raw.contains(private));
    }
}
