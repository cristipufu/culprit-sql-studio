use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabasePropertiesCase {
    Exact,
    ReadOnlyConnection,
    UnknownValues,
    NoRecordedCheckDb,
    UnknownCodes,
    NativePartial,
    Denied,
    NotApplicable,
}
impl DatabasePropertiesCase {
    pub const ALL: [Self; 8] = [
        Self::Exact,
        Self::ReadOnlyConnection,
        Self::UnknownValues,
        Self::NoRecordedCheckDb,
        Self::UnknownCodes,
        Self::NativePartial,
        Self::Denied,
        Self::NotApplicable,
    ];
}

/// Controlled native-shaped facts share one genuine collection/seal/readback harness; no live support is claimed.
pub fn database_properties_evidence(case: DatabasePropertiesCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut point: DatabasePropertiesObservation = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/database-properties.native.json"
    )))
    .unwrap();
    match case {
        DatabasePropertiesCase::ReadOnlyConnection => point.updateability = Some("READ_ONLY".into()),
        DatabasePropertiesCase::UnknownValues => {
            point.last_good_checkdb_without_offset = None;
            point.collation = None;
            point.compatibility_level = None;
            point.query_store_enabled = None;
            point.auto_create_statistics = None;
            point.auto_update_statistics = None;
            point.auto_update_statistics_async = None;
            point.target_recovery_time_seconds = None;
        }
        DatabasePropertiesCase::NoRecordedCheckDb => {
            point.last_good_checkdb_without_offset = Some("1900-01-01T00:00:00.000".into())
        }
        DatabasePropertiesCase::UnknownCodes => {
            point.catalog_state.code = Some(255);
            point.snapshot_isolation.code = None;
            point.updateability = Some("FUTURE_NATIVE_VALUE".into());
        }
        _ => {}
    }
    let denied = case == DatabasePropertiesCase::Denied;
    let not_applicable = case == DatabasePropertiesCase::NotApplicable;
    let records = if denied || not_applicable {
        vec![]
    } else {
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: DATABASE_PROPERTIES_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::DatabaseProperties(Box::new(point)),
        })]
    };
    let coverage = if denied {
        StateSourceCoverage::unavailable(
            DATABASE_PROPERTIES_SOURCE.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else if not_applicable {
        StateSourceCoverage::not_applicable(DATABASE_PROPERTIES_SOURCE.into())
    } else {
        StateSourceCoverage::observed(
            DATABASE_PROPERTIES_SOURCE.into(),
            1,
            case != DatabasePropertiesCase::NativePartial,
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
            records: records.len() as u32,
            sources: vec![coverage],
        }),
    };
    (source, records)
}
