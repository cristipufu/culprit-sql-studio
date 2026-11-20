use super::*;
fn native() -> NativeIdentity {
    NativeIdentity::new("7")
        .qualified("kind", "sql-server-local-database-recovery-epoch")
        .qualified("database_created_without_offset", "2019-01-01T00:00:00.0000000")
        .qualified("database_file_guid", "private-file")
        .qualified("database_guid", "private-database")
        .qualified("recovery_fork_guid", "private-fork")
        .qualified("instance_epoch", "private-process")
}
fn fixture_source() -> CollectionDescription {
    let native = native();
    CollectionDescription {
        target: crate::DatabaseIdentity::server(Engine::Mssql, "shop", "fixture.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("private-machine"),
                native: NativeIdentity::new("private-member"),
            }),
            epoch: Some(native.key().unwrap()),
        },
        source: "suspect-page-history-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::SuspectPages {
            native_database: Box::new(native),
            database_writable: true,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::SuspectPages,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(crate::source_id::SUSPECT_PAGES.into(), 1)],
            },
        },
    }
}
fn point() -> MssqlSuspectPageHistory {
    MssqlSuspectPageHistory {
        database: "shop".into(),
        native_database_id: "7".into(),
        observed_at: "2020-01-01T00:00:00.5000000Z".into(),
        database_writable: Some(true),
        count: "0".into(),
        retained_rows: "3".into(),
        last_reported_time_without_offset: None,
        reset: crate::ResetMarker::Unknown,
    }
}
fn row(point: MssqlSuspectPageHistory) -> OperationalObservation {
    OperationalObservation {
        source: crate::source_id::SUSPECT_PAGES.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::SuspectPages(Box::new(point)),
    }
}
#[test]
fn retained_repaired_rows_are_distinct_from_active_errors_and_bind_one_original_id() {
    let source = fixture_source();
    let row = row(point());
    assert!(row.is_valid());
    let read = SuspectPageHistoryRead::from_source(&source, &[&row], true).unwrap();
    assert_eq!(read.observation.as_ref().unwrap().active_count(), Some(0));
    assert_eq!(read.observation.as_ref().unwrap().retained_count(), Some(3));
    assert!(source.partition_is_consistent());
    let record = TelemetryRecord::Operational(row);
    ExportedRecord::new(&source, &CaptureIdentity { observation_id: "original".into(), revision: None }, record)
        .unwrap();
}
#[test]
fn foreign_ids_roles_source_families_clocks_and_missing_epochs_cannot_acquire_original_authority() {
    for change in ["id", "database", "clock", "role", "source", "dataset", "member", "epoch", "vendor"] {
        let mut source = fixture_source();
        let mut value = point();
        match change {
            "id" => value.native_database_id = "8".into(),
            "database" => value.database = "other".into(),
            "clock" => value.observed_at = "2020-01-01T00:00:02Z".into(),
            "role" => value.database_writable = Some(false),
            "member" => {
                source.partition = SourcePartition::InstanceLocal { member: None, epoch: Some(native().key().unwrap()) }
            }
            "epoch" => {
                if let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition {
                    *epoch = Some(digest(b"other-process"))
                }
            }
            "vendor" => {
                let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
                database.engine = Engine::Postgres;
                if let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition {
                    member.engine = Engine::Postgres;
                }
            }
            "dataset" => {
                let SourceObservation::SuspectPages { snapshot, .. } = &mut source.observation else { unreachable!() };
                snapshot.dataset = SnapshotDataset::Operational;
            }
            _ => {}
        }
        let mut row = row(value);
        if change == "source" {
            row.source = crate::source_id::LOG_CAPACITY.into();
        }
        assert!(
            ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                TelemetryRecord::Operational(row)
            )
            .is_err(),
            "{change}"
        );
    }
}
#[test]
fn null_or_absent_history_is_unknown_and_duplicate_or_impossible_counts_are_rejected() {
    let mut source = fixture_source();
    let SourceObservation::SuspectPages { snapshot, .. } = &mut source.observation else { unreachable!() };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::unavailable(
        crate::source_id::SUSPECT_PAGES.into(),
        crate::ErrorCode::PermissionDenied,
        None,
    );
    let read = SuspectPageHistoryRead::from_source(&source, &[], true).unwrap();
    assert!(read.observation.is_none());
    let source = fixture_source();
    let row = row(point());
    assert!(SuspectPageHistoryRead::from_source(&source, &[&row, &row], true).is_err());
    for counts in [("4", "3"), ("-1", "3"), ("01", "3"), ("0", "9223372036854775808")] {
        let mut point = point();
        point.count = counts.0.into();
        point.retained_rows = counts.1.into();
        assert!(!point.is_valid());
    }
}
#[test]
fn sharing_withdraws_the_entire_original_history_and_is_idempotent() {
    let source = fixture_source();
    let row = row(point());
    let read = SuspectPageHistoryRead::from_source(&source, &[&row], true).unwrap();
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.operations.suspect_pages = Some(Box::new(read));
    snapshot.sources.clear();
    let redacted = crate::redact::redact_snapshot(&snapshot);
    assert!(redacted.operations.suspect_pages.is_none());
    let source = redacted.sources.iter().find(|source| source.source == crate::source_id::SUSPECT_PAGES).unwrap();
    assert_eq!(source.state, SourceState::Unavailable);
    let json = serde_json::to_value(&redacted).unwrap();
    assert_eq!(json, serde_json::to_value(crate::redact::redact_snapshot(&redacted)).unwrap());
    assert!(!json.to_string().contains("private-process"));
}
