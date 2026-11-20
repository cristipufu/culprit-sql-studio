use super::*;
use crate::{Engine, ErrorCode, SourceState};
const DATABASE: &str = "10000000-0000-0000-0000-000000000001";
pub(crate) fn point() -> MssqlBackupHistoryObservation {
    let native = NativeIdentity::new("5")
        .qualified("kind", "sql-server-local-database-recovery-epoch")
        .qualified("database_created_without_offset", "2019-01-01T00:00:00.0000000")
        .qualified("database_file_guid", "native-file")
        .qualified("database_guid", DATABASE)
        .qualified("recovery_fork_guid", "native-fork")
        .qualified("instance_epoch", "native-process");
    let mut context = super::super::native_read_context::tests::read_context();
    let SourcePartition::InstanceLocal { epoch, .. } = &mut context.partition else { unreachable!() };
    *epoch = Some(digest(&serde_json::to_vec(&native).unwrap()));
    MssqlBackupHistoryObservation {
        scope: MssqlBackupHistoryScope {
            profile: MssqlBackupHistoryProfile::SqlServerMsdb,
            database_id: 5,
            native_database: native,
            hyperscale: None,
        },
        context,
        observed_at: "2020-01-01T12:00:00.5000000Z".into(),
        position: 1,
        record: MssqlBackupRecord::Msdb(Box::new(MssqlMsdbBackup {
            backup_set_id: 1,
            backup_set_uuid: "20000000-0000-0000-0000-000000000002".into(),
            database_guid: DATABASE.into(),
            database_name: "owned-db".into(),
            family_guid: None,
            first_recovery_fork_guid: None,
            last_recovery_fork_guid: None,
            started_without_offset: Some("2019-12-31T21:00:00.1230000".into()),
            finished_without_offset: None,
            time_zone_quarter_hours: Some(127),
            backup_type: Some(MssqlBackupType::Log),
            size_bytes: Some("18446744073709551616".into()),
            compressed_size_bytes: None,
            initiator_user_name: Some("historical-login".into()),
            server_name: None,
            machine_name: None,
            software: MssqlBackupSoftware { vendor_id: None, major: None, minor: None, build: None },
            lsns: MssqlBackupLsns {
                first: Some("9999999999999999999999999".into()),
                last: None,
                checkpoint: None,
                database_backup: None,
                differential_base: None,
                fork_point: None,
            },
            flags: MssqlBackupFlags {
                copy_only: None,
                snapshot: None,
                checksums: None,
                damaged: None,
                begins_log_chain: None,
                incomplete_metadata: None,
            },
        })),
    }
}
pub(crate) fn source(point: &MssqlBackupHistoryObservation) -> CollectionDescription {
    CollectionDescription {
        target: point.context.target.clone(),
        partition: point.context.partition.clone(),
        source: MSSQL_BACKUP_HISTORY_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::BackupHistory {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::BackupHistory,
                observed_from: point.context.observed_from.clone(),
                observed_until: point.context.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(MSSQL_BACKUP_HISTORY_SOURCE.into(), 1, false)],
            },
            scope: Box::new(point.scope.clone()),
            database_writable: point.context.database_writable.unwrap(),
        },
    }
}
fn row(point: MssqlBackupHistoryObservation) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: MSSQL_BACKUP_HISTORY_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlBackupHistory(Box::new(point)),
    })
}
fn exported(source: &CollectionDescription, point: MssqlBackupHistoryObservation) -> Result<ExportedRecord> {
    ExportedRecord::new(
        source,
        &CaptureIdentity { observation_id: "original-backups".into(), revision: None },
        row(point),
    )
}
#[test]
fn native_decimal_lsn_timezone_unknown_and_offset_free_clocks_round_trip_exactly() {
    let point = point();
    assert!(point.is_valid());
    let restored: MssqlBackupHistoryObservation = serde_json::from_slice(&serde_json::to_vec(&point).unwrap()).unwrap();
    assert_eq!(restored, point);
    assert!(exported(&source(&point), point.clone()).is_ok());
    let MssqlBackupRecord::Msdb(record) = restored.record else { unreachable!() };
    assert_eq!(record.size_bytes.as_deref(), Some("18446744073709551616"));
    assert_eq!(record.lsns.first.as_deref(), Some("9999999999999999999999999"));
    assert_eq!(record.time_zone_quarter_hours, Some(127));
    assert_eq!(record.finished_without_offset, None);
}
#[test]
fn raw_history_rows_require_exact_original_source_database_role_member_and_native_window() {
    let original = point();
    for change in ["source", "dataset", "member", "epoch", "role", "bounds", "scope"] {
        let mut changed = source(&original);
        match change {
            "source" => changed.source = "other-history".into(),
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    unreachable!()
                };
                member.native.id = "other-server".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { unreachable!() };
                *epoch = Some("other-process".into());
            }
            _ => {
                let SourceObservation::BackupHistory { snapshot, scope, database_writable } = &mut changed.observation
                else {
                    unreachable!()
                };
                match change {
                    "dataset" => snapshot.dataset = SnapshotDataset::Cleanup,
                    "role" => *database_writable = false,
                    "bounds" => snapshot.observed_until = "2020-01-01T12:00:02Z".into(),
                    _ => scope.database_id = 6,
                }
            }
        }
        assert!(exported(&changed, original.clone()).is_err(), "{change}");
    }
    let mut foreign = original.clone();
    let CollectionTarget::Database(database) = &mut foreign.context.target else { unreachable!() };
    database.engine = Engine::Postgres;
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut foreign.context.partition else {
        unreachable!()
    };
    member.engine = Engine::Postgres;
    assert!(foreign.context.has_qualified_local_owner());
    assert!(!foreign.is_valid());
}
#[test]
fn malformed_nullable_fields_are_not_coerced_to_zero_current_owner_or_utc() {
    for change in ["size", "lsn", "zone", "clock", "subtick", "database", "negative", "label", "position", "future"] {
        let mut changed = point();
        match change {
            "position" => changed.position = MAX_MSSQL_BACKUP_HISTORY_ROWS + 1,
            "future" => changed.observed_at = "2020-01-01T12:00:02Z".into(),
            _ => {
                let MssqlBackupRecord::Msdb(record) = &mut changed.record else { unreachable!() };
                match change {
                    "size" => record.size_bytes = Some("184467440737095516160".into()),
                    "negative" => record.size_bytes = Some("-1".into()),
                    "lsn" => record.lsns.first = Some("1e23".into()),
                    "zone" => record.time_zone_quarter_hours = Some(49),
                    "clock" => record.started_without_offset = Some("2019-12-31T21:00:00.123000Z".into()),
                    "subtick" => record.started_without_offset = Some("2019-12-31T21:00:00.12300001".into()),
                    "database" => record.database_guid = "other-database".into(),
                    _ => record.initiator_user_name = Some("界".repeat(129)),
                }
            }
        }
        assert!(!changed.is_valid(), "{change}");
        assert!(changed.key().is_err(), "{change}");
    }
}
#[test]
fn empty_or_unavailable_header_still_requires_original_database_epoch_and_refuses_complete_feed() {
    let point = point();
    for state in [SourceState::Partial, SourceState::Unavailable] {
        let mut source = source(&point);
        let SourceObservation::BackupHistory { snapshot, .. } = &mut source.observation else { unreachable!() };
        snapshot.records = 0;
        snapshot.sources = vec![if state == SourceState::Partial {
            StateSourceCoverage::observed(MSSQL_BACKUP_HISTORY_SOURCE.into(), 0, false)
        } else {
            StateSourceCoverage::unavailable(MSSQL_BACKUP_HISTORY_SOURCE.into(), ErrorCode::PermissionDenied, None)
        }];
        assert!(backup_history_context(&source).is_ok());
        for change in ["guid", "fork", "epoch", "complete", "bare"] {
            let mut wrong = source.clone();
            let SourceObservation::BackupHistory { snapshot, scope, .. } = &mut wrong.observation else {
                unreachable!()
            };
            match change {
                "guid" => {
                    scope.native_database.qualifiers.remove("database_guid");
                }
                "fork" => {
                    scope.native_database.qualifiers.remove("recovery_fork_guid");
                }
                "epoch" => wrong.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
                "complete" => {
                    snapshot.sources = vec![StateSourceCoverage::collected(MSSQL_BACKUP_HISTORY_SOURCE.into(), 0)]
                }
                _ => wrong.observation = SourceObservation::Snapshot(snapshot.clone()),
            }
            assert!(!wrong.partition_is_consistent(), "{state:?}/{change}");
        }
    }
}
#[test]
fn azure_requires_native_logical_guid_and_known_conventional_deployment_for_every_available_row() {
    let mut point = point();
    let CollectionTarget::Database(database) = &mut point.context.target else { unreachable!() };
    database.incarnation = Some(DATABASE.into());
    point.context.partition = SourcePartition::Target;
    point.scope = MssqlBackupHistoryScope {
        profile: MssqlBackupHistoryProfile::AzureSqlDatabase,
        database_id: 5,
        native_database: NativeIdentity::new(DATABASE)
            .qualified("kind", "azure-sql-logical-database")
            .qualified("physical_database_guid", "native-physical"),
        hyperscale: Some(false),
    };
    point.record = MssqlBackupRecord::AzureSqlDatabase(Box::new(MssqlAzureSqlBackup {
        backup_file_id: "native-backup".into(),
        logical_database_id: DATABASE.into(),
        physical_database_name: "physical-db".into(),
        logical_server_name: "logical-server".into(),
        logical_database_name: "owned-db".into(),
        started_without_offset: "2019-12-31T21:00:00.1234567".into(),
        finished_without_offset: "2019-12-31T21:01:00.7654321".into(),
        backup_type: MssqlBackupType::Database,
        in_retention: false,
    }));
    assert!(exported(&source(&point), point.clone()).is_ok());
    for hyperscale in [None, Some(true)] {
        let mut unknown = point.clone();
        unknown.scope.hyperscale = hyperscale;
        assert!(!unknown.is_valid());
        let mut unavailable = source(&unknown);
        let SourceObservation::BackupHistory { snapshot, .. } = &mut unavailable.observation else { unreachable!() };
        snapshot.records = 0;
        snapshot.sources =
            vec![StateSourceCoverage::unavailable(MSSQL_BACKUP_HISTORY_SOURCE.into(), ErrorCode::Unsupported, None)];
        assert!(backup_history_context(&unavailable).is_ok());
    }
    let MssqlBackupRecord::AzureSqlDatabase(record) = &mut point.record else { unreachable!() };
    record.logical_database_id = "foreign-logical-database".into();
    assert!(!point.is_valid());
}
#[test]
fn native_backup_identity_is_stable_on_replay_but_does_not_merge_different_capture_receipts() {
    let point = point();
    let source = source(&point);
    let first = exported(&source, point.clone()).unwrap();
    assert_eq!(first, exported(&source, point.clone()).unwrap());
    let other = ExportedRecord::new(
        &source,
        &CaptureIdentity { observation_id: "later-capture".into(), revision: None },
        row(point.clone()),
    )
    .unwrap();
    assert_ne!(first.record_id, other.record_id);
    let mut second = point.record.clone();
    let MssqlBackupRecord::Msdb(record) = &mut second else { unreachable!() };
    record.backup_set_uuid = "other-backup-uuid".into();
    assert!(
        !backup_history_roster_is_unique([&point.record, &second]),
        "same native primary key cannot name two UUIDs"
    );
}
#[test]
fn nullable_native_contract_fields_are_required_on_deserialization() {
    let original = point();
    for field in ["time_zone_quarter_hours", "finished_without_offset", "size_bytes", "initiator_user_name"] {
        let mut value = serde_json::to_value(&original).unwrap();
        value["record"]["data"].as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlBackupHistoryObservation>(value).is_err(), "{field}");
    }
}

#[test]
fn specialized_backup_header_matches_only_its_declared_snapshot_request_and_original_row_budget() {
    let point = point();
    let original = source(&point);
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::BackupHistory },
        budget: CollectionBudget { max_snapshot_rows: 1, ..Default::default() },
    };
    assert!(original.matches_request(&request));
    let wrong = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Cleanup },
        ..request.clone()
    };
    assert!(!original.matches_request(&wrong));
    let mut too_many = original.clone();
    let SourceObservation::BackupHistory { snapshot, .. } = &mut too_many.observation else { unreachable!() };
    snapshot.records = 2;
    snapshot.sources[0].records = 2;
    assert!(too_many.partition_is_consistent());
    assert!(!too_many.matches_request(&request));
    let mut bare = original;
    let SourceObservation::BackupHistory { snapshot, .. } = bare.observation else { unreachable!() };
    bare.observation = SourceObservation::Snapshot(snapshot);
    assert!(!bare.matches_request(&request), "a generic header cannot establish the original backup scope");
}
