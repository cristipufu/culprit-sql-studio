use super::*;
use culprit_core::{DatabaseIdentity, Engine};
fn original() -> (CollectionDescription, Vec<OperationalObservation>) {
    let native = NativeIdentity::new("logical-guid")
        .qualified("kind", "azure-sql-logical-database")
        .qualified("physical_database_guid", "physical-guid");
    let mut database = DatabaseIdentity::server(Engine::Mssql, "original", "original.invalid", 1433, None);
    database.incarnation = Some(native.id.clone());
    let context = NativeReadContext {
        target: database.into(),
        partition: SourcePartition::Target,
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(true),
    };
    let scope = MssqlBackupHistoryScope {
        profile: MssqlBackupHistoryProfile::AzureSqlDatabase,
        database_id: 7,
        native_database: native,
        hyperscale: Some(false),
    };
    let point = MssqlBackupHistoryObservation {
        context: context.clone(),
        scope: scope.clone(),
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        position: 1,
        record: MssqlBackupRecord::AzureSqlDatabase(Box::new(MssqlAzureSqlBackup {
            backup_file_id: "native-file".into(),
            logical_database_id: "logical-guid".into(),
            physical_database_name: "native-physical".into(),
            logical_server_name: "native-server".into(),
            logical_database_name: "historical-name".into(),
            started_without_offset: "2019-12-31T20:00:00.1234567".into(),
            finished_without_offset: "2019-12-31T20:01:00.7654321".into(),
            backup_type: MssqlBackupType::Database,
            in_retention: false,
        })),
    };
    let row = OperationalObservation {
        source: MSSQL_BACKUP_HISTORY_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlBackupHistory(Box::new(point)),
    };
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: MSSQL_BACKUP_HISTORY_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::BackupHistory {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::BackupHistory,
                observed_from: context.observed_from,
                observed_until: context.observed_until,
                records: 1,
                sources: vec![StateSourceCoverage::observed(MSSQL_BACKUP_HISTORY_SOURCE.into(), 1, false)],
            },
            scope: Box::new(scope),
            database_writable: true,
        },
    };
    (source, vec![row])
}
fn report(
    source: &CollectionDescription,
    rows: &[OperationalObservation],
    read_complete: bool,
) -> Result<BackupHistoryReport> {
    explain(BackupHistoryEvidence { source, observations: &rows.iter().collect::<Vec<_>>(), read_complete })
}
#[test]
fn retained_native_record_preserves_exact_fields_and_informational_advice_without_restore_or_io_cause_claim() {
    let (source, rows) = original();
    let report = report(&source, &rows, true).unwrap();
    let OperationalFact::MssqlBackupHistory(point) = &rows[0].fact else { unreachable!() };
    assert_eq!(report.entries[0].native, point.record);
    assert_eq!(report.entries[0].observation_key, rows[0].key);
    assert_eq!(report.entries[0].title, t!("view.backup_history.entry.database"));
    assert_eq!(report.entries[0].explanation, t!("view.backup_history.entry.recorded"));
    assert_eq!(report.entries[0].investigation, t!("view.backup_history.investigate"));
    assert_eq!(report.entries[0].verify, t!("view.backup_history.verify"));
    assert_eq!(report.source_state, SourceState::Partial);
    assert!(!report.source.complete_scope);
    assert_eq!(
        report.limitations,
        vec![
            t!("view.backup_history.limit.retained"),
            t!("view.backup_history.limit.clocks"),
            t!("view.backup_history.limit.recoverability"),
            t!("view.backup_history.limit.azure_retention")
        ]
    );
}
#[test]
fn empty_partial_denied_and_hyperscale_keep_distinct_native_qualifications() {
    for state in ["empty", "warehouse-gap", "denied", "hyperscale", "unknown-tier"] {
        let (mut source, _) = original();
        let SourceObservation::BackupHistory { snapshot, scope, .. } = &mut source.observation else { unreachable!() };
        if state != "warehouse-gap" {
            snapshot.records = 0;
            snapshot.sources[0].records = 0;
        }
        if matches!(state, "denied" | "hyperscale" | "unknown-tier") {
            snapshot.sources[0] = StateSourceCoverage::unavailable(
                MSSQL_BACKUP_HISTORY_SOURCE.into(),
                if state == "denied" { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported },
                None,
            );
        }
        if state == "hyperscale" {
            scope.hyperscale = Some(true);
        } else if state == "unknown-tier" {
            scope.hyperscale = None;
        }
        let report = report(&source, &[], state != "warehouse-gap").unwrap();
        assert!(report.entries.is_empty());
        assert_eq!(
            report.source_state,
            if matches!(state, "denied" | "hyperscale" | "unknown-tier") {
                SourceState::Unavailable
            } else {
                SourceState::Partial
            }
        );
        assert_eq!(
            report.summary,
            if report.source_state == SourceState::Unavailable {
                t!("view.backup_history.summary.unavailable")
            } else {
                t!("view.backup_history.summary.empty")
            }
        );
        let qualifier = match state {
            "warehouse-gap" => Some(t!("view.backup_history.limit.partial")),
            "denied" => Some(t!("view.backup_history.limit.permission")),
            "hyperscale" => Some(t!("view.backup_history.limit.hyperscale")),
            "unknown-tier" => Some(t!("view.backup_history.limit.deployment")),
            _ => None,
        };
        if let Some(qualifier) = qualifier {
            assert!(report.limitations.contains(&qualifier));
        }
    }
}
#[test]
fn foreign_header_duplicate_native_keys_or_missing_complete_body_cannot_lend_advice() {
    let (source, rows) = original();
    assert!(report(&source, &[], true).is_err());
    let mut repeated = rows.clone();
    repeated.push(rows[0].clone());
    assert!(report(&source, &repeated, false).is_err());
    for change in ["scope", "bounds", "source", "dataset", "role"] {
        let mut foreign = source.clone();
        let SourceObservation::BackupHistory { snapshot, scope, database_writable } = &mut foreign.observation else {
            unreachable!()
        };
        match change {
            "scope" => scope.native_database.id = "other-guid".into(),
            "bounds" => snapshot.observed_until = "2020-01-01T00:00:02Z".into(),
            "source" => foreign.source = "other-source".into(),
            "dataset" => snapshot.dataset = SnapshotDataset::Cdc,
            _ => *database_writable = false,
        }
        assert!(report(&foreign, &rows, true).is_err(), "{change}");
    }
}
