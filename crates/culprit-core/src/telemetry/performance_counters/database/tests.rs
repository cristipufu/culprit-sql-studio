use super::*;
use crate::{DatabaseIdentity, Engine, MeasurementScope, ResetMarker};

fn native() -> NativeIdentity {
    NativeIdentity::new("7")
        .qualified("kind", "sql-server-local-database-recovery-epoch")
        .qualified("database_created_without_offset", "2019-01-01T00:00:00.0000000")
        .qualified("database_file_guid", "private-file-guid")
        .qualified("database_guid", "private-database-guid")
        .qualified("recovery_fork_guid", "private-fork-guid")
        .qualified("instance_epoch", "private-process")
}
fn source() -> CollectionDescription {
    let native = native();
    CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mssql, "shop", "fixture.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("private-machine"),
                native: NativeIdentity::new("private-member"),
            }),
            epoch: Some(native.key().unwrap()),
        },
        source: "database-performance-counter-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::DatabasePerformanceCounters {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::DatabasePerformanceCounters,
                observed_from: "2020-01-01T12:00:00.000000000Z".into(),
                observed_until: "2020-01-01T12:00:01.000000000Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(crate::source_id::PERFORMANCE_COUNTERS.into(), 1)],
            },
            native_database: Box::new(native),
            database_writable: true,
        },
    }
}
fn sample() -> NativeCounterSet {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let mut counters = vec![];
    for (i, field) in profile.database_fields().enumerate() {
        let descriptor = (format!("SQLServer:{}", field.object_family.unwrap()), field.native_name, Some("shop"));
        let (nature, unit) = field.meaning(272_696_576);
        counters.push(NativeCounterSample {
            key: digest(&serde_json::to_vec(&descriptor).unwrap()),
            native_object: descriptor.0,
            native_name: field.native_name.into(),
            native_instance: Some("shop".into()),
            native_type: Some("272696576".into()),
            scope: MeasurementScope::Database,
            reset: ResetMarker::Unknown,
            nature,
            unit,
            value: MeasurementValue::Integer(if i == 0 { "9007199254740993".into() } else { "0".into() }),
        });
    }
    NativeCounterSet {
        identity: native(),
        scope: MeasurementScope::Database,
        counters,
        ratios: vec![],
        ratio_gaps: vec![],
        properties: std::collections::BTreeMap::from([
            (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), DATABASE_PERFORMANCE_COUNTER_PROFILE.into()),
            (PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:00.500000000Z".into()),
            (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "3".into()),
            (DATABASE_PERFORMANCE_NAME_PROPERTY.into(), "shop".into()),
            ("catalog_visibility".into(), "complete".into()),
            ("catalog_name_matches".into(), "1".into()),
        ]),
    }
}
fn export(source: &CollectionDescription, sample: NativeCounterSet) -> Result<ExportedRecord> {
    ExportedRecord::new(
        source,
        &CaptureIdentity { observation_id: "original-db-counters".into(), revision: None },
        TelemetryRecord::Operational(OperationalObservation::from_counters(
            crate::source_id::PERFORMANCE_COUNTERS,
            sample,
        )?),
    )
}
#[test]
fn exact_database_population_preserves_native_key_wide_values_and_unknown_reset_through_export() {
    let source = source();
    let sample = sample();
    assert!(source.partition_is_consistent());
    assert_eq!(sample.key().unwrap(), sample.identity.key().unwrap());
    assert_eq!(sample.key().unwrap(), digest(&serde_json::to_vec(&sample.identity).unwrap()));
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    assert!(context.has_qualified_local_owner());
    assert_eq!(context.database_writable, Some(true));
    assert!(context.target.database().unwrap().incarnation.is_none());
    let row = export(&source, sample.clone()).unwrap();
    assert_eq!(serde_json::from_str::<ExportedRecord>(&serde_json::to_string(&row).unwrap()).unwrap(), row);
    assert_eq!(sample.counters[0].value.integer(), Some(9_007_199_254_740_993));
    assert!(
        sample.counters.iter().all(|row| row.scope == MeasurementScope::Database && row.reset == ResetMarker::Unknown)
    );
}
#[test]
fn database_body_cannot_borrow_shared_total_foreign_names_times_or_native_database_proof() {
    for cause in [
        "total",
        "foreign-name",
        "wrong-scope",
        "wrong-guid",
        "wrong-clock",
        "meaning",
        "duplicate",
        "duplicate-field",
        "catalog",
        "visibility",
        "ratios",
    ] {
        let mut value = sample();
        match cause {
            "total" => value.counters[0].native_instance = Some("_Total".into()),
            "foreign-name" => {
                value.properties.insert(DATABASE_PERFORMANCE_NAME_PROPERTY.into(), "other".into());
            }
            "wrong-scope" => value.scope = MeasurementScope::Server,
            "wrong-guid" => {
                value.identity.qualifiers.insert("database_guid".into(), "other".into());
            }
            "wrong-clock" => {
                value
                    .properties
                    .insert(PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:01.000000001Z".into());
            }
            "meaning" => value.counters[0].nature = CounterNature::Gauge,
            "duplicate" => {
                value.counters[1].native_name = value.counters[0].native_name.clone();
            }
            "duplicate-field" => {
                value.counters[1].native_name = value.counters[0].native_name.clone();
                value.counters[1].native_object = "MSSQL$other:Databases".into();
            }
            "catalog" => {
                value.properties.insert("catalog_name_matches".into(), "2".into());
            }
            "visibility" => {
                value.properties.remove("catalog_visibility");
            }
            _ => value.ratios.push(NativeCounterRatio {
                kind: CounterRatioKind::PlanCacheHit,
                numerator: None,
                base: None,
                support: CounterRatioSupport::Supported,
                observed_at: None,
            }),
        }
        assert!(export(&source(), value).is_err(), "{cause}");
    }
}
#[test]
fn original_header_refuses_target_scope_missing_epoch_role_and_generic_snapshot_substitutes() {
    for cause in ["target", "member", "epoch", "guid", "role", "generic", "foreign-engine", "empty"] {
        let mut source = source();
        match cause {
            "target" => source.partition = SourcePartition::Target,
            "member" => {
                let SourcePartition::InstanceLocal { member, .. } = &mut source.partition else { panic!() };
                *member = None;
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                *epoch = Some("another-original".into());
            }
            "generic" => {
                source.observation = SourceObservation::Snapshot(source.observation.snapshot().unwrap().clone());
            }
            "foreign-engine" => {
                let CollectionTarget::Database(database) = &mut source.target else { panic!() };
                database.engine = Engine::Postgres;
            }
            _ => {
                let SourceObservation::DatabasePerformanceCounters { native_database, database_writable, snapshot } =
                    &mut source.observation
                else {
                    panic!()
                };
                match cause {
                    "guid" => {
                        native_database.qualifiers.remove("database_guid");
                    }
                    "role" => *database_writable = false,
                    _ => {
                        snapshot.records = 0;
                        snapshot.sources =
                            vec![StateSourceCoverage::collected(crate::source_id::PERFORMANCE_COUNTERS.into(), 0)];
                    }
                }
            }
        }
        assert!(!source.partition_is_consistent(), "{cause}");
        assert!(export(&source, sample()).is_err(), "{cause}");
    }
}
#[test]
fn unavailable_original_header_retains_known_database_and_never_establishes_zero_or_wide_name_binding() {
    let mut source = source();
    let SourceObservation::DatabasePerformanceCounters { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = 0;
    snapshot.sources = vec![StateSourceCoverage::unavailable(
        crate::source_id::PERFORMANCE_COUNTERS.into(),
        crate::ErrorCode::PermissionDenied,
        None,
    )];
    assert!(source.partition_is_consistent());
    assert!(NativeReadContext::from_snapshot(&source).unwrap().has_qualified_local_owner());
    for name in ["", " shop", "shop ", "_Total", "_tOtAl", &"x".repeat(128), &"𐐀".repeat(64)] {
        assert!(!database_performance_name_is_admissible(name));
    }
    assert!(database_performance_name_is_admissible(&"𐐀".repeat(63)));
    assert!(database_performance_name_is_admissible("数据库"));
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::DatabasePerformanceCounters },
        budget: CollectionBudget::default(),
    };
    assert!(source.matches_request(&request));
    assert_eq!(SnapshotDataset::DatabasePerformanceCounters.streams(), &[TelemetryStream::OperationalSamples]);
    let mut wrong = request;
    wrong.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational };
    assert!(!source.matches_request(&wrong));
}
