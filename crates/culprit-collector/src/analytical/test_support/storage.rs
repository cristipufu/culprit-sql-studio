use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageCase {
    Exact,
    ReadOnlyOffline,
    Unknown,
    Partial,
    Denied,
}
impl StorageCase {
    pub const ALL: [Self; 5] = [Self::Exact, Self::ReadOnlyOffline, Self::Unknown, Self::Partial, Self::Denied];
}
/// Native-shaped controlled facts reuse genuine collection/sealing/publication. They are not live qualification.
pub fn storage_evidence(case: StorageCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
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
