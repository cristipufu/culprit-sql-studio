use super::*;
use culprit_collector::analytical::test_support::{
    BackupHistoryCase, ColumnstoreRowgroupCase, backup_history_evidence, columnstore_rowgroup_evidence,
};
use culprit_core::SourceState;
async fn snapshot(
    reader: &OutboxCaptureReader,
    id: &str,
    connection: &str,
    kind: LocalCaptureAnalysisKind,
) -> Result<LocalSnapshotAnalysis> {
    let operation = OperationGuard::new(reader.timeout()).unwrap();
    let value = reader
        .analyze(LocalCaptureRequest { connection, observation_id: id, analysis: kind, operation: operation.handle() })
        .await?;
    let LocalCapturedAnalysis::Snapshot(value) = value else { panic!("typed canonical snapshot report") };
    Ok(value)
}
fn reader(fixture: &LocalCaptureFixture) -> OutboxCaptureReader {
    OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
        .unwrap()
}
#[tokio::test]
async fn selected_rowgroups_keep_the_original_target_selection_and_every_native_outcome_in_one_local_read() {
    for case in ColumnstoreRowgroupCase::ALL {
        let (source, rows) = columnstore_rowgroup_evidence(case);
        let SourceObservation::ColumnstoreRowgroups { target, .. } = &source.observation else { unreachable!() };
        let target = target.clone();
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        assert_eq!(
            fixture.manifest.request.selection,
            CollectionSelection::ColumnstoreRowgroups { target: target.clone() }
        );
        let analysis = snapshot(
            &reader(&fixture),
            &fixture.manifest.capture.observation_id,
            "local-evidence",
            LocalCaptureAnalysisKind::ColumnstoreRowgroups,
        )
        .await
        .unwrap();
        let LocalSnapshotReport::ColumnstoreRowgroups(report) = analysis.report() else { unreachable!() };
        assert_eq!(report.target, *target);
        assert_eq!(analysis.original().read().description, source);
        assert!(analysis.original().read().is_valid());
        let canonical = culprit_checks::columnstore_rowgroups::explain_captured(analysis.original().read()).unwrap();
        assert_eq!(serde_json::to_value(report).unwrap(), serde_json::to_value(canonical).unwrap(), "{case:?}");
        assert!(report.read_complete);
        assert_eq!(report.source.state, source.observation.snapshot().unwrap().sources[0].state);
        assert_eq!(fixture.manifest.source_queries, 0);
    }
}
#[tokio::test]
async fn backup_profiles_keep_empty_unavailable_nullable_and_partial_retained_history_after_local_authentication() {
    for case in BackupHistoryCase::ALL {
        let (source, rows) = backup_history_evidence(case);
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        let analysis = snapshot(
            &reader(&fixture),
            &fixture.manifest.capture.observation_id,
            "local-evidence",
            LocalCaptureAnalysisKind::BackupHistory,
        )
        .await
        .unwrap();
        let LocalSnapshotReport::BackupHistory(report) = analysis.report() else { unreachable!() };
        assert_eq!(**report, culprit_checks::backup_history::explain_captured(analysis.original().read()).unwrap());
        assert_eq!(analysis.original().read().description, source);
        assert!(report.read_complete);
        assert!(!report.source.complete_scope);
        assert_eq!(
            report.source_state,
            if matches!(
                case,
                BackupHistoryCase::PermissionDenied
                    | BackupHistoryCase::Hyperscale
                    | BackupHistoryCase::UnknownDeployment
            ) {
                SourceState::Unavailable
            } else {
                SourceState::Partial
            }
        );
        assert_eq!(fixture.manifest.source_queries, 0);
    }
}
#[tokio::test]
async fn neither_kind_can_relabel_another_dataset_or_borrow_a_different_saved_connection() {
    for kind in [LocalCaptureAnalysisKind::ColumnstoreRowgroups, LocalCaptureAnalysisKind::BackupHistory] {
        let (source, rows) = if kind == LocalCaptureAnalysisKind::ColumnstoreRowgroups {
            columnstore_rowgroup_evidence(ColumnstoreRowgroupCase::States)
        } else {
            backup_history_evidence(BackupHistoryCase::MsdbNative)
        };
        let fixture = LocalCaptureFixture::snapshot(source, rows).await;
        let reader = reader(&fixture);
        let id = &fixture.manifest.capture.observation_id;
        let wrong = if kind == LocalCaptureAnalysisKind::ColumnstoreRowgroups {
            LocalCaptureAnalysisKind::BackupHistory
        } else {
            LocalCaptureAnalysisKind::ColumnstoreRowgroups
        };
        assert_eq!(
            snapshot(&reader, id, "local-evidence", wrong).await.unwrap_err().detail,
            Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
        );
        assert_eq!(
            snapshot(&reader, id, "other-connection", kind).await.unwrap_err().detail,
            Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture))
        );
        assert!(snapshot(&reader, id, "local-evidence", kind).await.is_ok());
    }
}
#[tokio::test]
async fn a_missing_sealed_native_body_fails_fresh_local_verification_instead_of_reusing_a_report() {
    for kind in [LocalCaptureAnalysisKind::ColumnstoreRowgroups, LocalCaptureAnalysisKind::BackupHistory] {
        let (source, rows) = if kind == LocalCaptureAnalysisKind::ColumnstoreRowgroups {
            columnstore_rowgroup_evidence(ColumnstoreRowgroupCase::States)
        } else {
            backup_history_evidence(BackupHistoryCase::MultipleRecords)
        };
        let fixture = LocalCaptureFixture::snapshot(source, rows).await;
        let reader = reader(&fixture);
        let id = &fixture.manifest.capture.observation_id;
        let original = snapshot(&reader, id, "local-evidence", kind).await.unwrap();
        assert!(original.original().read().is_valid());
        let native =
            fixture.manifest.files.iter().find(|file| file.stream == TelemetryStream::OperationalSamples).unwrap();
        std::fs::remove_file(fixture.capture_directory.join(&native.file)).unwrap();
        assert!(snapshot(&reader, id, "local-evidence", kind).await.is_err());
        assert!(
            original.original().read().is_valid(),
            "the returned original remains its immutable structural receipt, not a new read"
        );
    }
}
