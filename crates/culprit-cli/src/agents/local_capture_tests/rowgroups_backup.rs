use super::*;
use culprit_collector::analytical::test_support::{
    BackupHistoryCase, ColumnstoreRowgroupCase, backup_history_evidence, columnstore_rowgroup_evidence,
};
use culprit_engine::LocalCaptureAnalysisKind;
use serde_json::Value;

fn cases() -> Vec<(LocalCaptureAnalysisKind, CollectionDescription, Vec<TelemetryRecord>)> {
    ColumnstoreRowgroupCase::ALL
        .into_iter()
        .map(|case| {
            let (source, rows) = columnstore_rowgroup_evidence(case);
            (LocalCaptureAnalysisKind::ColumnstoreRowgroups, source, rows)
        })
        .chain(BackupHistoryCase::ALL.into_iter().map(|case| {
            let (source, rows) = backup_history_evidence(case);
            (LocalCaptureAnalysisKind::BackupHistory, source, rows)
        }))
        .collect()
}

#[tokio::test]
async fn canonical_rowgroups_and_backup_history_reach_real_mcp_with_original_values_and_finite_source_qualifications() {
    for (kind, source, rows) in cases() {
        let native = source.observation.snapshot().unwrap().clone();
        let (summary, limitations) = report_words(kind, &source, &rows);
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), rows.clone()).await;
        let manifest_before = std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) = analyze(&server, &fixture.manifest.capture.observation_id, kind).await;
        assert!(!error, "{kind:?}: {value}");
        let name = serde_json::to_value(kind).unwrap();
        let report = &value[name.as_str().unwrap()];
        assert_eq!(report["summary"], summary);
        assert_eq!(report["limitations"], json!(limitations));
        assert_eq!(report["read_complete"], true);
        assert_eq!(report["source_state"], json!(native.sources[0].state));
        assert_eq!(value["native_rows"], native.records);
        assert_eq!(value["observed_rows"], native.records);
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["original_read_bounds"], json!({"from":native.observed_from,"until":native.observed_until}));
        let entries = report["entries"].as_array().unwrap();
        assert_eq!(entries.len(), rows.len().min(5));
        match &source.observation {
            SourceObservation::ColumnstoreRowgroups { target, .. } => assert_rowgroups(&fixture, target, &rows, report),
            SourceObservation::BackupHistory { scope, .. } => assert_backups(scope, &native, &rows, report),
            _ => unreachable!(),
        }
        assert_references(&fixture, &source, &rows, &manifest_before, entries);
        assert_private_proof_is_withheld(&value);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(analyze(&server, &fixture.manifest.capture.observation_id, kind).await.1, value);
        assert_eq!(std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap(), manifest_before);
    }
}

fn report_words(
    kind: LocalCaptureAnalysisKind,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
) -> (String, Vec<String>) {
    let observations = rows
        .iter()
        .map(|row| match row {
            TelemetryRecord::Operational(row) => row,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    match kind {
        LocalCaptureAnalysisKind::ColumnstoreRowgroups => {
            let report = culprit_checks::columnstore_rowgroups::explain(
                culprit_checks::columnstore_rowgroups::ColumnstoreRowgroupEvidence {
                    source,
                    observations: &observations,
                    read_complete: true,
                },
            )
            .unwrap();
            (report.summary, report.limitations)
        }
        LocalCaptureAnalysisKind::BackupHistory => {
            let report =
                culprit_checks::backup_history::explain(culprit_checks::backup_history::BackupHistoryEvidence {
                    source,
                    observations: &observations,
                    read_complete: true,
                })
                .unwrap();
            (report.summary, report.limitations)
        }
        _ => unreachable!(),
    }
}

fn assert_rowgroups(
    fixture: &LocalCaptureFixture,
    target: &MssqlColumnstoreTarget,
    rows: &[TelemetryRecord],
    report: &Value,
) {
    let entries = report["entries"].as_array().unwrap();
    assert_eq!(
        fixture.manifest.request.selection,
        CollectionSelection::ColumnstoreRowgroups { target: Box::new(target.clone()) }
    );
    assert_eq!(
        report["selected_target"],
        json!({"database_id":target.table.database_id,"object_id":target.table.object_id,"index_id":target.index_id,"partition_number":target.partition_number})
    );
    for (entry, row) in entries.iter().zip(rows) {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlColumnstoreRowgroup(point),
            ..
        }) = row
        else {
            unreachable!()
        };
        assert_eq!(entry["partition_number"], point.partition_number);
        assert_eq!(entry["row_group_id"], point.row_group_id);
        assert_eq!(entry["state"], point.state);
        assert_eq!(entry["observed_at"], point.observed_at);
        let metric = entry["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|metric| metric["label"] == culprit_core::t!("view.columnstore_rowgroups.bytes"))
            .unwrap();
        assert_eq!(
            metric["text"],
            point
                .size_bytes
                .as_ref()
                .map(|value| culprit_core::format::digits(value.clone()))
                .unwrap_or_else(|| culprit_core::t!("view.columnstore_rowgroups.unknown"))
        );
    }
}

fn assert_backups(
    scope: &MssqlBackupHistoryScope,
    native: &StateSnapshotDescription,
    rows: &[TelemetryRecord],
    report: &Value,
) {
    let entries = report["entries"].as_array().unwrap();
    assert_eq!(report["profile"], json!(scope.profile));
    assert_eq!(
        report["native_source"],
        json!({"state":native.sources[0].state,"records":native.records,"complete_scope":false,"error_code":native.sources[0].error_code,"stopped":native.sources[0].stopped})
    );
    for (entry, row) in entries.iter().zip(rows) {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlBackupHistory(point),
            ..
        }) = row
        else {
            unreachable!()
        };
        let record = &entry["native_record"];
        match &point.record {
            MssqlBackupRecord::AzureSqlDatabase(backup) => {
                assert_eq!(record["backup_type"], json!(backup.backup_type));
                assert_eq!(record["started_without_offset"], backup.started_without_offset);
                assert_eq!(record["finished_without_offset"], backup.finished_without_offset);
                assert_eq!(record["in_retention"], backup.in_retention);
                assert_eq!(record["physical_database_name"], backup.physical_database_name);
                assert_eq!(record["logical_server_name"], backup.logical_server_name);
                assert_eq!(record["logical_database_name"], backup.logical_database_name);
            }
            MssqlBackupRecord::Msdb(backup) => {
                assert_eq!(record["backup_set_id"], backup.backup_set_id);
                assert_eq!(record["backup_type"], json!(backup.backup_type));
                assert_eq!(record["started_without_offset"], json!(backup.started_without_offset));
                assert_eq!(record["finished_without_offset"], json!(backup.finished_without_offset));
                assert_eq!(record["time_zone_quarter_hours"], json!(backup.time_zone_quarter_hours));
                assert_eq!(record["size_bytes"], json!(backup.size_bytes));
                assert_eq!(record["compressed_size_bytes"], json!(backup.compressed_size_bytes));
                assert_eq!(record["lsns"], json!(backup.lsns));
                assert_eq!(record["flags"], json!(backup.flags));
                assert_eq!(record["historical_initiator_user_name"], json!(backup.initiator_user_name));
            }
        }
    }
}

fn assert_references(
    fixture: &LocalCaptureFixture,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
    manifest: &[u8],
    entries: &[Value],
) {
    let manifest = digest(manifest);
    let scope = source.captured_range().unwrap().scope_id;
    let observation = &fixture.manifest.capture.observation_id;
    for (entry, row) in entries.iter().zip(rows) {
        let TelemetryRecord::Operational(row) = row else { unreachable!() };
        assert_eq!(
            entry["reference"],
            digest(format!("local-snapshot-entry:{manifest}:{scope}:{observation}:{}", row.key).as_bytes())
        );
    }
}
fn assert_private_proof_is_withheld(value: &Value) {
    let text = value.to_string();
    for private in [
        "manifest_sha256",
        "source_sha256",
        "payload_sha256",
        "qualifiers",
        "native-instance",
        "native-machine",
        "original.invalid",
        "original-native-process",
        "10000000-0000-0000-0000-000000000001",
        "20000000-0000-0000-0000-000000000002",
        "30000000-0000-0000-0000-000000000003",
        "40000000-0000-0000-0000-000000000004",
        "50000000-0000-0000-0000-000000000005",
        "60000000-0000-0000-0000-000000000006",
    ] {
        assert!(!text.contains(private), "{private}");
    }
    assert!(text.len() <= 64 * 1024);
}

#[tokio::test]
async fn wrong_dataset_and_missing_originals_refuse_both_new_mcp_analyses_without_source_work() {
    for kind in [LocalCaptureAnalysisKind::ColumnstoreRowgroups, LocalCaptureAnalysisKind::BackupHistory] {
        let (source, rows) = if kind == LocalCaptureAnalysisKind::ColumnstoreRowgroups {
            columnstore_rowgroup_evidence(ColumnstoreRowgroupCase::States)
        } else {
            backup_history_evidence(BackupHistoryCase::MultipleRecords)
        };
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let id = &fixture.manifest.capture.observation_id;
        let wrong = if kind == LocalCaptureAnalysisKind::ColumnstoreRowgroups {
            LocalCaptureAnalysisKind::BackupHistory
        } else {
            LocalCaptureAnalysisKind::ColumnstoreRowgroups
        };
        let (error, value) = analyze(&server, id, wrong).await;
        assert!(error);
        assert!(value.get(serde_json::to_value(wrong).unwrap().as_str().unwrap()).is_none());
        assert!(!analyze(&server, id, kind).await.0);
        let native =
            fixture.manifest.files.iter().find(|file| file.stream == TelemetryStream::OperationalSamples).unwrap();
        std::fs::remove_file(fixture.capture_directory.join(&native.file)).unwrap();
        let (error, value) = analyze(&server, id, kind).await;
        assert!(error);
        assert!(value.get(serde_json::to_value(kind).unwrap().as_str().unwrap()).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn five_entry_presentation_limits_do_not_rewrite_native_counts_selection_or_backup_history_coverage() {
    for kind in [LocalCaptureAnalysisKind::ColumnstoreRowgroups, LocalCaptureAnalysisKind::BackupHistory] {
        let (mut source, rows) = if kind == LocalCaptureAnalysisKind::ColumnstoreRowgroups {
            columnstore_rowgroup_evidence(ColumnstoreRowgroupCase::States)
        } else {
            backup_history_evidence(BackupHistoryCase::MsdbNative)
        };
        let rows = (0..7)
            .map(|index| {
                let TelemetryRecord::Operational(mut row) = rows[0].clone() else { unreachable!() };
                match &mut row.fact {
                    OperationalFact::MssqlColumnstoreRowgroup(point) => {
                        point.row_group_id = index;
                        row.key = point.key().unwrap();
                    }
                    OperationalFact::MssqlBackupHistory(point) => {
                        point.position = index + 1;
                        let MssqlBackupRecord::Msdb(backup) = &mut point.record else { unreachable!() };
                        backup.backup_set_id = index + 1;
                        backup.backup_set_uuid = format!("90000000-0000-0000-0000-{index:012}");
                        row.key = point.key().unwrap();
                    }
                    _ => unreachable!(),
                }
                TelemetryRecord::Operational(row)
            })
            .collect::<Vec<_>>();
        let header = match &mut source.observation {
            SourceObservation::ColumnstoreRowgroups { snapshot, .. }
            | SourceObservation::BackupHistory { snapshot, .. } => snapshot,
            _ => unreachable!(),
        };
        header.records = 7;
        header.sources[0].records = 7;
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), rows.clone()).await;
        let manifest = std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) = analyze(&server, &fixture.manifest.capture.observation_id, kind).await;
        assert!(!error, "{value}");
        let name = serde_json::to_value(kind).unwrap();
        let report = &value[name.as_str().unwrap()];
        assert_eq!(value["native_rows"], 7);
        assert_eq!(value["observed_rows"], 7);
        assert_eq!(value["read_complete"], true);
        assert_eq!(report["entries_available"], 7);
        assert_eq!(report["entries_presented"], 5);
        assert_eq!(report["entries_omitted"], 2);
        assert_eq!(report["entries_limited"], true);
        let entries = report["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 5);
        assert_references(&fixture, &source, &rows, &manifest, entries);
        if kind == LocalCaptureAnalysisKind::BackupHistory {
            assert_eq!(report["source_state"], json!(SourceState::Partial));
            assert_eq!(report["native_source"]["complete_scope"], false);
        } else {
            assert_eq!(report["selected_target"]["partition_number"], 1);
        }
        assert_private_proof_is_withheld(&value);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
