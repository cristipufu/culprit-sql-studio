//! One synthetic original native-backup corpus for SDK and both offline destinations. No native cost proof.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupHistoryCase {
    AzureNative,
    MsdbNative,
    ManagedInstance,
    NullableNative,
    DamagedNative,
    MultipleRecords,
    Empty,
    PermissionDenied,
    Hyperscale,
    UnknownDeployment,
    RowLimited,
}
impl BackupHistoryCase {
    pub const ALL: [Self; 11] = [
        Self::AzureNative,
        Self::MsdbNative,
        Self::ManagedInstance,
        Self::NullableNative,
        Self::DamagedNative,
        Self::MultipleRecords,
        Self::Empty,
        Self::PermissionDenied,
        Self::Hyperscale,
        Self::UnknownDeployment,
        Self::RowLimited,
    ];
}
const DATABASE_GUID: &str = "10000000-0000-0000-0000-000000000001";
const BACKUP_UUID: &str = "20000000-0000-0000-0000-000000000002";
const RECOVERY_FORK: &str = "30000000-0000-0000-0000-000000000003";
pub fn backup_history_evidence(case: BackupHistoryCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let azure = matches!(
        case,
        BackupHistoryCase::AzureNative | BackupHistoryCase::Hyperscale | BackupHistoryCase::UnknownDeployment
    );
    let profile = if azure {
        MssqlBackupHistoryProfile::AzureSqlDatabase
    } else if case == BackupHistoryCase::ManagedInstance {
        MssqlBackupHistoryProfile::ManagedInstanceMsdb
    } else {
        MssqlBackupHistoryProfile::SqlServerMsdb
    };
    let native_database = if azure {
        NativeIdentity::new(DATABASE_GUID)
            .qualified("kind", "azure-sql-logical-database")
            .qualified("physical_database_guid", "40000000-0000-0000-0000-000000000004")
    } else {
        NativeIdentity::new("5")
            .qualified("kind", "sql-server-local-database-recovery-epoch")
            .qualified("database_created_without_offset", "2019-01-01T04:00:00.0000000")
            .qualified("database_file_guid", "50000000-0000-0000-0000-000000000005")
            .qualified("database_guid", DATABASE_GUID)
            .qualified("recovery_fork_guid", RECOVERY_FORK)
            .qualified("instance_epoch", "original-native-process")
    };
    let mut database = DatabaseIdentity::server(Engine::Mssql, "original-db", "original.invalid", 1433, None);
    if azure {
        database.incarnation = Some(DATABASE_GUID.into());
    }
    let partition = if azure {
        SourcePartition::Target
    } else {
        SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("native-machine"),
                native: NativeIdentity::new("native-instance"),
            }),
            epoch: Some(digest(&serde_json::to_vec(&native_database).unwrap())),
        }
    };
    let context = NativeReadContext {
        target: database.into(),
        partition,
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(true),
    };
    let scope = MssqlBackupHistoryScope {
        profile,
        database_id: 5,
        native_database,
        hyperscale: if case == BackupHistoryCase::UnknownDeployment {
            None
        } else if azure {
            Some(case == BackupHistoryCase::Hyperscale)
        } else {
            None
        },
    };
    let unavailable = match case {
        BackupHistoryCase::PermissionDenied => Some(ErrorCode::PermissionDenied),
        BackupHistoryCase::Hyperscale => Some(ErrorCode::Unsupported),
        BackupHistoryCase::UnknownDeployment => Some(ErrorCode::QueryFailed),
        _ => None,
    };
    let mut records = Vec::new();
    if unavailable.is_none() && case != BackupHistoryCase::Empty {
        let record = if azure {
            MssqlBackupRecord::AzureSqlDatabase(Box::new(MssqlAzureSqlBackup {
                backup_file_id: BACKUP_UUID.into(),
                logical_database_id: DATABASE_GUID.into(),
                physical_database_name: "native-physical-db".into(),
                logical_server_name: "native-logical-server".into(),
                logical_database_name: "original-db".into(),
                started_without_offset: "2019-12-31T21:00:00.1234567".into(),
                finished_without_offset: "2019-12-31T21:00:01.7654321".into(),
                backup_type: MssqlBackupType::Database,
                in_retention: false,
            }))
        } else {
            let nullable = case == BackupHistoryCase::NullableNative;
            MssqlBackupRecord::Msdb(Box::new(MssqlMsdbBackup {
                backup_set_id: 7,
                backup_set_uuid: BACKUP_UUID.into(),
                database_guid: DATABASE_GUID.into(),
                database_name: "original-db".into(),
                family_guid: Some("60000000-0000-0000-0000-000000000006".into()),
                first_recovery_fork_guid: Some(RECOVERY_FORK.into()),
                last_recovery_fork_guid: Some(RECOVERY_FORK.into()),
                started_without_offset: (!nullable).then(|| "2019-12-31T21:00:00.1230000".into()),
                finished_without_offset: (!nullable).then(|| "2019-12-31T21:00:01.0000000".into()),
                time_zone_quarter_hours: (!nullable).then_some(127),
                backup_type: (!nullable).then_some(MssqlBackupType::Database),
                size_bytes: (!nullable).then(|| "18446744073709551616".into()),
                compressed_size_bytes: None,
                initiator_user_name: (!nullable).then(|| "historical-login".into()),
                server_name: None,
                machine_name: None,
                software: MssqlBackupSoftware {
                    vendor_id: Some(4608),
                    major: Some(16),
                    minor: Some(0),
                    build: Some(4325),
                },
                lsns: MssqlBackupLsns {
                    first: Some("1844674407370955161600000".into()),
                    last: Some("1844674407370955161600010".into()),
                    checkpoint: None,
                    database_backup: None,
                    differential_base: None,
                    fork_point: None,
                },
                flags: MssqlBackupFlags {
                    copy_only: Some(true),
                    snapshot: None,
                    checksums: Some(true),
                    damaged: Some(case == BackupHistoryCase::DamagedNative),
                    begins_log_chain: None,
                    incomplete_metadata: None,
                },
            }))
        };
        let point = MssqlBackupHistoryObservation {
            context: context.clone(),
            scope: scope.clone(),
            observed_at: "2020-01-01T00:00:00.5Z".into(),
            position: 1,
            record,
        };
        records.push(TelemetryRecord::Operational(OperationalObservation {
            source: MSSQL_BACKUP_HISTORY_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MssqlBackupHistory(Box::new(point)),
        }));
    }
    if case == BackupHistoryCase::MultipleRecords {
        let TelemetryRecord::Operational(first) = &records[0] else { unreachable!() };
        let OperationalFact::MssqlBackupHistory(first) = &first.fact else { unreachable!() };
        let mut next = first.as_ref().clone();
        next.position = 2;
        let MssqlBackupRecord::Msdb(backup) = &mut next.record else { unreachable!() };
        backup.backup_set_id = 6;
        backup.backup_set_uuid = "70000000-0000-0000-0000-000000000007".into();
        backup.backup_type = Some(MssqlBackupType::Log);
        backup.flags.copy_only = Some(false);
        records.push(TelemetryRecord::Operational(OperationalObservation {
            source: MSSQL_BACKUP_HISTORY_SOURCE.into(),
            key: next.key().unwrap(),
            fact: OperationalFact::MssqlBackupHistory(Box::new(next)),
        }));
    }
    let mut coverage = if let Some(code) = unavailable {
        StateSourceCoverage::unavailable(MSSQL_BACKUP_HISTORY_SOURCE.into(), code, None)
    } else {
        StateSourceCoverage::observed(MSSQL_BACKUP_HISTORY_SOURCE.into(), records.len() as u32, false)
    };
    if case == BackupHistoryCase::RowLimited {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
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
                records: records.len() as u32,
                sources: vec![coverage],
            },
            scope: Box::new(scope),
            database_writable: true,
        },
    };
    assert!(backup_history_context(&source).is_ok());
    (source, records)
}
pub fn assert_backup_history_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, rows);
    assert!(backup_history_context(capture.source()).is_ok());
    assert!(capture.source().observation.snapshot().unwrap().sources.iter().all(|source| !source.complete_scope));
    for row in capture.records() {
        let TelemetryRecord::Operational(observation) = &row.row().record else { panic!("native backup record") };
        let OperationalFact::MssqlBackupHistory(point) = &observation.fact else { panic!("native backup point") };
        assert!(point.fits_source(source));
        assert_eq!(
            point.record,
            match &rows[point.position as usize - 1] {
                TelemetryRecord::Operational(row) => match &row.fact {
                    OperationalFact::MssqlBackupHistory(point) => point.record.clone(),
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            }
        );
    }
}
