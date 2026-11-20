//! One exact original population, shared by SDK and both physical warehouse codecs.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabasePerformanceCounterCase {
    Exact,
    UnknownNativeType,
    Partial,
    Denied,
}
impl DatabasePerformanceCounterCase {
    pub const ALL: [Self; 4] = [Self::Exact, Self::UnknownNativeType, Self::Partial, Self::Denied];
}
pub fn database_performance_counter_evidence(
    case: DatabasePerformanceCounterCase,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let native = NativeIdentity::new("7")
        .qualified("kind", "sql-server-local-database-recovery-epoch")
        .qualified("database_created_without_offset", "2019-01-01T00:00:00.0000000")
        .qualified("database_guid", "private-database-guid-canary")
        .qualified("recovery_fork_guid", "private-fork-guid-canary")
        .qualified("database_file_guid", "private-file-guid-canary")
        .qualified("instance_epoch", "private-process-canary");
    let mut records = vec![];
    if case != DatabasePerformanceCounterCase::Denied {
        let profile = Engine::Mssql.spec().performance_counters().unwrap();
        let mut counters = vec![];
        for (index, field) in profile.database_fields().enumerate() {
            if case == DatabasePerformanceCounterCase::Partial && index == 2 {
                continue;
            }
            let native_type =
                if case == DatabasePerformanceCounterCase::UnknownNativeType && index == 0 { 42 } else { 272_696_576 };
            let (nature, unit) = field.meaning(native_type);
            let object = format!("SQLServer:{}", field.object_family.unwrap());
            let descriptor = (&object, field.native_name, Some("shop"));
            counters.push(NativeCounterSample {
                key: digest(&serde_json::to_vec(&descriptor).unwrap()),
                value: MeasurementValue::Integer(if index == 0 { "9007199254740993".into() } else { "0".into() }),
                unit,
                nature,
                scope: MeasurementScope::Database,
                reset: ResetMarker::Unknown,
                native_object: object,
                native_name: field.native_name.into(),
                native_instance: Some("shop".into()),
                native_type: Some(native_type.to_string()),
            });
        }
        let set = NativeCounterSet {
            identity: native.clone(),
            scope: MeasurementScope::Database,
            properties: std::collections::BTreeMap::from([
                (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), DATABASE_PERFORMANCE_COUNTER_PROFILE.into()),
                (PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:00.500000000Z".into()),
                (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), counters.len().to_string()),
                (DATABASE_PERFORMANCE_NAME_PROPERTY.into(), "shop".into()),
                ("catalog_visibility".into(), "complete".into()),
                ("catalog_name_matches".into(), "1".into()),
            ]),
            counters,
            ratios: vec![],
            ratio_gaps: vec![],
        };
        profile.validate(&set).unwrap();
        records.push(TelemetryRecord::Operational(
            OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, set).unwrap(),
        ));
    }
    let count = records.len() as u32;
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "shop", "fixture.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("private-machine-canary"),
                native: NativeIdentity::new("private-member-canary"),
            }),
            epoch: Some(native.key().unwrap()),
        },
        source: "database-performance-counter-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::DatabasePerformanceCounters {
            native_database: Box::new(native),
            database_writable: true,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::DatabasePerformanceCounters,
                observed_from: "2020-01-01T12:00:00.000000000Z".into(),
                observed_until: "2020-01-01T12:00:01.000000000Z".into(),
                records: count,
                sources: vec![if case == DatabasePerformanceCounterCase::Denied {
                    StateSourceCoverage::unavailable(
                        culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
                        culprit_core::ErrorCode::PermissionDenied,
                        None,
                    )
                } else {
                    StateSourceCoverage::observed(
                        culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
                        count,
                        case != DatabasePerformanceCounterCase::Partial,
                    )
                }],
            },
        },
    };
    assert!(source.partition_is_consistent());
    (source, records)
}
