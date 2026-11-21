use super::*;
use culprit_collector::analytical::test_support::{TableMemoryCase, table_memory_evidence};
use culprit_core::{Report, TableMemoryReport};
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
                analysis: LocalCaptureAnalysisKind::TableMemory,
                operation: guard.handle(),
            },
        )
        .await
        .unwrap();
    let LocalCapturedAnalysis::Snapshot(result) = result else { panic!() };
    result
}
fn reports(report: &Report) -> &[TableMemoryReport] {
    report.table_memory.as_deref().unwrap()
}
fn assert_original(report: &Report, source: &CollectionDescription) {
    let [RecordedOperationalRead::Captured { read }] = report.snapshot.operations.table_memory.as_slice() else {
        panic!()
    };
    assert!(read.is_valid());
    assert_eq!(&read.description, source);
    assert_eq!(reports(report)[0].original.as_ref().unwrap().context, read.context().unwrap());
    assert_eq!(reports(report)[0].source.state, source.observation.snapshot().unwrap().sources[0].state);
    assert!(reports(report)[0].limitations.contains(&culprit_core::t!("view.table_memory.limit.historical")));
    assert_eq!(
        report.evidence.as_ref().unwrap().collected.as_ref().unwrap().operations.table_memory,
        report.snapshot.operations.table_memory
    );
}
#[tokio::test]
async fn selected_table_memory_attaches_after_native_shutdown_and_replays_without_original_files_or_credentials() {
    for case in TableMemoryCase::ALL {
        let (source, rows) = table_memory_evidence(case);
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
        let result = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_populations_with_completed_capture(
                    "shop",
                    &options(),
                    CompletedCaptureScanInput {
                        analysis: CompletedCaptureAnalysis::TableMemory,
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
                .all(|report| report.snapshot.operations.table_memory.is_empty() && report.table_memory.is_none())
        );
        let expected = serde_json::to_value(&result.requested.table_memory).unwrap();
        let id = result.requested.snapshot.id.clone();
        drop(captured);
        let replay = workspace.report_at("shop", &id).unwrap();
        assert_original(&replay, &source);
        assert_eq!(serde_json::to_value(&replay.table_memory).unwrap(), expected);
        assert_eq!(serde_json::to_value(culprit_engine::reports::display(&replay)).unwrap()["table_memory"], expected);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn distinct_table_incarnations_members_and_captures_coexist_without_current_name_or_database_join() {
    for changed in ["new-capture", "object", "creation", "member", "route"] {
        let (source, rows) = table_memory_evidence(TableMemoryCase::Exact);
        let first = Captured::from_source("shop", source, rows).await;
        let (mut source, mut rows) = table_memory_evidence(TableMemoryCase::Partial);
        let SourceObservation::TableMemory { target, .. } = &mut source.observation else { panic!() };
        if changed == "object" {
            target.object_id += 1;
        }
        if changed == "creation" {
            target.created_without_offset = "2020-01-01T00:00:00.0000000".into();
        }
        let TelemetryRecord::Operational(row) = &mut rows[0] else { panic!() };
        let OperationalFact::MssqlTableMemory(point) = &mut row.fact else { panic!() };
        point.table = target.as_ref().clone();
        row.key = point.key().unwrap();
        if changed == "member" {
            let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else { panic!() };
            member.native.id = "original-other-member".into();
        }
        if changed == "route" {
            let CollectionTarget::Database(database) = &mut source.target else { panic!() };
            database.database = "old-database".into();
            database.location =
                DatabaseLocation::Server { host: "old-route.invalid".into(), port: 1444, instance: None };
        }
        let second = Captured::from_source("shop", source.clone(), rows).await;
        let mut batch = snapshots();
        let (_home, preparation, _) = workspace(batch.clone(), SourceShutdown::Quiescent);
        let first_analysis = analyze(&preparation, &first).await;
        batch
            .requested
            .operations
            .table_memory
            .push(RecordedOperationalRead::Captured { read: Box::new(first_analysis.original().read().clone()) });
        let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
        let second_analysis = analyze(&workspace, &second).await;
        let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace.scan_with_captured_table_memory("shop", &options(), None, second_analysis, operation).await
        })
        .await
        .unwrap();
        assert_eq!(report.snapshot.operations.table_memory.len(), 2);
        assert_eq!(reports(&report).len(), 2);
        assert_eq!(reports(&report)[1].original.as_ref().unwrap().context.partition, source.partition);
        assert_eq!(reports(&report)[1].original.as_ref().unwrap().context.target, source.target);
        assert_eq!(reports(&report)[1].source.state, SourceState::Partial);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
        assert!(report.findings.iter().all(|finding| finding.check != "table-memory"));
    }
}
#[tokio::test]
async fn wrong_saved_connection_cancelled_original_or_unconfirmed_scan_never_publishes_table_memory() {
    for cause in ["connection", "cancelled", "unconfirmed"] {
        let (source, rows) = table_memory_evidence(TableMemoryCase::Exact);
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
                            analysis: CompletedCaptureAnalysis::TableMemory,
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
                    .scan_with_captured_table_memory(
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
    let (source, rows) = table_memory_evidence(TableMemoryCase::Exact);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, _) = workspace(snapshots(), SourceShutdown::Quiescent);
    let original = analyze(&workspace, &captured).await;
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_captured_table_memory("shop", &options(), None, original, operation).await
    })
    .await
    .unwrap();
    let shared = culprit_core::redact::redact_snapshot(&report.snapshot);
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&shared).unwrap()).unwrap());
    assert_eq!(reports(&replay).len(), 1);
    assert!(reports(&replay)[0].original.is_none());
    assert_eq!(reports(&replay)[0].source.state, SourceState::Unavailable);
    assert_eq!(reports(&replay)[0].native_rows, None);
    assert!(reports(&replay)[0].metrics.is_empty());
    assert!(matches!(
        replay.snapshot.operations.table_memory.as_slice(),
        [RecordedOperationalRead::Unavailable { .. }]
    ));
    let text = serde_json::to_string(&replay.snapshot.operations.table_memory).unwrap();
    for private in [
        "9007199254740993",
        "private-machine-canary",
        "private-instance-canary",
        "private-process-and-database-canary",
        "manifest_sha256",
    ] {
        assert!(!text.contains(private), "{private}");
    }
}
