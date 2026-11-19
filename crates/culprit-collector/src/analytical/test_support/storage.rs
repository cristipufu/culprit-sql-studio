use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageCase {
    Exact,
    ReadOnlyOffline,
    Unknown,
    Partial,
    Denied,
    SharedTempdbLog,
    SharedTempdbLogUnknown,
    SharedTempdbLogPartial,
    SharedTempdbLogDenied,
}
impl StorageCase {
    pub const ALL: [Self; 9] = [
        Self::Exact,
        Self::ReadOnlyOffline,
        Self::Unknown,
        Self::Partial,
        Self::Denied,
        Self::SharedTempdbLog,
        Self::SharedTempdbLogUnknown,
        Self::SharedTempdbLogPartial,
        Self::SharedTempdbLogDenied,
    ];
}
/// Native-shaped controlled facts reuse genuine collection/sealing/publication. They are not live qualification.
pub fn storage_evidence(case: StorageCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    if matches!(
        case,
        StorageCase::SharedTempdbLog
            | StorageCase::SharedTempdbLogUnknown
            | StorageCase::SharedTempdbLogPartial
            | StorageCase::SharedTempdbLogDenied
    ) {
        return shared_tempdb_log(case);
    }
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/storage.native.json")))
            .unwrap();
    let mut space: OperationalSpace = serde_json::from_value(fixture["space"].clone()).unwrap();
    let mut file: MssqlDatabaseFile = serde_json::from_value(fixture["file"].clone()).unwrap();
    if case == StorageCase::ReadOnlyOffline {
        file.is_read_only = Some(true);
        file.filegroup_read_only = Some(true);
        file.native_state = "6".into();
        file.state_description = Some("OFFLINE".into());
        file.used_pages = None;
    }
    if case == StorageCase::Unknown {
        space.used_bytes = None;
        file.size_basis = StorageFileSizeBasis::Unknown;
        file.native_type = "255".into();
        file.kind = culprit_core::FileKind::Other;
        file.type_description = None;
        file.used_pages = None;
        file.database_is_snapshot = None;
        file.growth_is_percent = None;
    }
    let rows = if case == StorageCase::Denied {
        vec![]
    } else {
        vec![
            TelemetryRecord::Operational(OperationalObservation {
                source: culprit_core::source_id::FILE_CAPACITY.into(),
                key: "filegroup:1".into(),
                fact: OperationalFact::Space(space),
            }),
            TelemetryRecord::Operational(OperationalObservation {
                source: culprit_core::source_id::FILE_CAPACITY.into(),
                key: file.key().unwrap(),
                fact: OperationalFact::MssqlDatabaseFile(Box::new(file)),
            }),
        ]
    };
    let coverage = if case == StorageCase::Denied {
        StateSourceCoverage::unavailable(
            culprit_core::source_id::FILE_CAPACITY.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed(
            culprit_core::source_id::FILE_CAPACITY.into(),
            rows.len() as u32,
            case != StorageCase::Partial,
        )
    };
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "sample",
            "fixture.invalid",
            1433,
            Some("native-logical-database".into()),
        )
        .into(),
        partition: SourcePartition::Target,
        source: "operational-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: rows.len() as u32,
            sources: vec![coverage],
        }),
    };
    (source, rows)
}

fn shared_tempdb_log(case: StorageCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, rows) = storage_evidence(StorageCase::Exact);
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::Space(mut data), .. }) =
        rows.into_iter().next().unwrap()
    else {
        unreachable!()
    };
    data.population = StoragePopulation::SharedTempdbDataFiles;
    data.kind = culprit_core::FileKind::Temp;
    data.scope = culprit_core::FileScope::Server;
    data.database = None;
    data.native_name = None;
    data.total_bytes = Some("1048576".into());
    data.used_bytes = Some("4096".into());
    data.limit_bytes = None;
    data.growth_enabled = None;
    data.reuse_wait = None;
    data.parts.clear();
    let mut log = data.clone();
    log.population = StoragePopulation::SharedTempdbTransactionLog;
    log.kind = culprit_core::FileKind::Log;
    log.native_name = Some("tempdb".into());
    log.total_bytes = Some("9007199254740993".into());
    log.used_bytes = Some("1".into());
    log.limit_bytes = Some("9007199254740993".into());
    log.growth_enabled = Some(false);
    log.reuse_wait = Some("ACTIVE_TRANSACTION".into());
    if case == StorageCase::SharedTempdbLogUnknown {
        log.used_bytes = None;
        log.limit_bytes = None;
        log.growth_enabled = None;
        log.reuse_wait = None;
    }
    let mut rows = vec![TelemetryRecord::Operational(OperationalObservation {
        source: culprit_core::source_id::TEMP_CAPACITY.into(),
        key: "shared-tempdb".into(),
        fact: OperationalFact::Space(data),
    })];
    if case != StorageCase::SharedTempdbLogDenied {
        rows.push(TelemetryRecord::Operational(OperationalObservation {
            source: culprit_core::source_id::LOG_CAPACITY.into(),
            key: "shared-tempdb-log".into(),
            fact: OperationalFact::Space(log),
        }));
    }
    let owner = SharedResourceIdentity {
        engine: culprit_core::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("original-native-tempdb-machine")
            .qualified("kind", "sql-server-network-machine"),
        native: NativeIdentity::new("original-native-tempdb-instance").qualified("kind", "sql-server-instance"),
    };
    source.target = CollectionTarget::Shared(owner.clone());
    source.partition =
        SourcePartition::InstanceLocal { member: Some(owner), epoch: Some(digest(b"original-tempdb-process")) };
    source.source = "shared-operational-state".into();
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.records = rows.len() as u32;
    snapshot.sources = vec![
        StateSourceCoverage::collected(culprit_core::source_id::TEMP_CAPACITY.into(), 1),
        if case == StorageCase::SharedTempdbLogDenied {
            StateSourceCoverage::unavailable(
                culprit_core::source_id::LOG_CAPACITY.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            )
        } else {
            StateSourceCoverage::observed(
                culprit_core::source_id::LOG_CAPACITY.into(),
                1,
                case != StorageCase::SharedTempdbLogPartial,
            )
        },
    ];
    (source, rows)
}
