use super::*;
use culprit_collector::analytical::test_support::{GaleraCase, galera_evidence};
use culprit_core::Report;
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureRequest, LocalCapturedAnalysis, LocalSnapshotAnalysis, OutboxCaptureReader,
};
async fn analyze(workspace: &Workspace, captured: &Captured) -> LocalSnapshotAnalysis {
    let reader = OutboxCaptureReader::delivery(captured.outbox.delivery(), CompletedCaptureBudget::default()).unwrap();
    let guard = culprit_core::limits::OperationGuard::new(std::time::Duration::from_secs(5)).unwrap();
    let result = workspace
        .analyze_local_capture(
            &reader,
            LocalCaptureRequest {
                connection: "shop",
                observation_id: &captured.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::Galera,
                operation: guard.handle(),
            },
        )
        .await
        .unwrap();
    let LocalCapturedAnalysis::Snapshot(result) = result else { panic!() };
    result
}

fn assert_original(report: &Report, source: &CollectionDescription) {
    let [RecordedOperationalRead::Captured { read }] = report.snapshot.operations.galera.as_slice() else { panic!() };
    assert!(read.is_valid());
    assert_eq!(&read.description, source);
    let [group] = report.galera.as_deref().unwrap() else { panic!() };
    assert_eq!(group.original.as_ref().unwrap().context, read.context().unwrap());
    assert!(group.limitations.contains(&culprit_core::t!("view.galera.non_atomic")));
    assert_eq!(
        report.evidence.as_ref().unwrap().collected.as_ref().unwrap().operations.galera,
        report.snapshot.operations.galera
    );
}
#[tokio::test]
async fn native_group_original_attaches_only_after_scan_settlement_and_replays_without_source_files_or_credentials() {
    for case in GaleraCase::ALL {
        let (source, rows) = galera_evidence(case);
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
        let result = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_populations_with_completed_capture(
                    "shop",
                    &options(),
                    CompletedCaptureScanInput {
                        analysis: CompletedCaptureAnalysis::Galera,
                        ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
                    },
                    operation,
                )
                .await
        })
        .await
        .unwrap();
        assert_original(&result.requested, &source);
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
        assert!(
            result
                .populations
                .iter()
                .all(|report| report.snapshot.operations.galera.is_empty() && report.galera.is_none())
        );
        let expected = serde_json::to_value(&result.requested.galera).unwrap();
        let id = result.requested.snapshot.id.clone();
        drop(captured);
        let replay = workspace.report_at("shop", &id).unwrap();
        assert_original(&replay, &source);
        assert_eq!(serde_json::to_value(&replay.galera).unwrap(), expected);
        assert_eq!(serde_json::to_value(culprit_engine::reports::display(&replay)).unwrap()["galera"], expected);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn independently_scoped_groups_and_members_coexist_without_borrowing_current_scan_database_or_route() {
    let (source, rows) = galera_evidence(GaleraCase::Queued);
    let first = Captured::from_source("shop", source.clone(), rows).await;
    let mut batch = snapshots();
    let (_home, preparation, _) = workspace(batch.clone(), SourceShutdown::Quiescent);
    let prior = analyze(&preparation, &first).await;
    batch
        .requested
        .operations
        .galera
        .push(RecordedOperationalRead::Captured { read: Box::new(prior.original().read().clone()) });
    let (mut next, rows) = galera_evidence(GaleraCase::Denied);
    let SourcePartition::InstanceLocal { member: Some(member), epoch } = &mut next.partition else { panic!() };
    member.native.id = "other-original-observer".into();
    *epoch = Some("other-original-process".into());
    next.target = CollectionTarget::Shared(member.clone());
    let second = Captured::from_source("shop", next.clone(), rows).await;
    let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
    let original = analyze(&workspace, &second).await;
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_captured_galera("shop", &options(), None, original, operation).await
    })
    .await
    .unwrap();
    let reports = report.galera.as_deref().unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].original.as_ref().unwrap().context.partition, source.partition);
    assert_eq!(reports[1].original.as_ref().unwrap().context.partition, next.partition);
    assert_eq!(reports[1].original.as_ref().unwrap().context.target, next.target);
    assert_eq!(reports[1].sources[0].state, SourceState::Unavailable);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn wrong_saved_connection_cancelled_original_or_unconfirmed_scan_never_publishes_galera() {
    for cause in ["connection", "cancelled", "unconfirmed"] {
        let (source, rows) = galera_evidence(GaleraCase::Queued);
        let captured = Captured::from_source("shop", source, rows).await;
        let (_home, workspace, state) = workspace(
            snapshots(),
            if cause == "unconfirmed" { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent },
        );
        let result = if cause == "cancelled" {
            let cancellation = CollectionCancellation::default();
            cancellation.cancel();
            crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
                workspace
                    .scan_with_completed_capture(
                        "shop",
                        &options(),
                        CompletedCaptureScanInput {
                            analysis: CompletedCaptureAnalysis::Galera,
                            ..captured.input(&captured.outbox.delivery(), &cancellation)
                        },
                        operation,
                    )
                    .await
            })
            .await
        } else {
            let original = analyze(&workspace, &captured).await;
            crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
                workspace
                    .scan_with_captured_galera(
                        if cause == "connection" { "foreign" } else { "shop" },
                        &options(),
                        None,
                        original,
                        operation,
                    )
                    .await
            })
            .await
        };
        assert!(result.is_err(), "{cause}");
        assert_eq!(state.factories.load(Ordering::SeqCst), usize::from(cause == "unconfirmed"));
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}
#[tokio::test]
async fn sharing_reanalysis_keeps_each_unavailable_original_status_without_native_owner_or_measurements() {
    let (source, rows) = galera_evidence(GaleraCase::Queued);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, _) = workspace(snapshots(), SourceShutdown::Quiescent);
    let original = analyze(&workspace, &captured).await;
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_captured_galera("shop", &options(), None, original, operation).await
    })
    .await
    .unwrap();
    let shared = culprit_core::redact::redact_snapshot(&report.snapshot);
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&shared).unwrap()).unwrap());
    assert_eq!(replay.galera.as_deref().unwrap().len(), 1);
    assert!(replay.galera.as_deref().unwrap()[0].original.is_none());
    assert_eq!(replay.galera.as_deref().unwrap()[0].sources[0].state, SourceState::Unavailable);
    assert_eq!(replay.galera.as_deref().unwrap()[0].native_rows, None);
    assert!(replay.galera.as_deref().unwrap()[0].entries.is_empty());
    assert!(matches!(replay.snapshot.operations.galera.as_slice(), [RecordedOperationalRead::Unavailable { .. }]));
    let text = serde_json::to_string(&replay.snapshot.operations.galera).unwrap();
    for private in [
        "9007199254740993",
        "aaaaaaaa-0000-4000-8000-000000000001",
        "bbbbbbbb-0000-4000-8000-000000000002",
        "private-galera-process-canary",
        "manifest_sha256",
    ] {
        assert!(!text.contains(private), "{private}");
    }
}
