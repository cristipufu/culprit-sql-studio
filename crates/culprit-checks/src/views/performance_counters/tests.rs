use super::*;
use culprit_core::{MeasurementScope, PerformanceCounterAvailability, ResetMarker};
use std::collections::BTreeMap;

#[test]
fn original_governor_rows_and_request_marker_refuse_another_member_process_or_clock() {
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-network"),
        native: NativeIdentity::new("native-instance"),
    };
    let epoch = digest(b"native-process");
    let context = NativeReadContext {
        target: CollectionTarget::Shared(member.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(member.clone()), epoch: Some(epoch.clone()) },
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        database_writable: None,
    };
    let instance = member.telemetry_id().unwrap();
    let mut marked = sample();
    marked.properties.extend([
        (MSSQL_RESOURCE_GOVERNOR_REQUEST_PROPERTY.into(), "true".into()),
        ("resource_governor_instance_id".into(), instance.clone()),
        ("resource_governor_instance_epoch".into(), epoch.clone()),
        ("resource_governor_native_rows".into(), "1".into()),
    ]);
    let name = "same-as-database-label";
    let mut governed = sample();
    governed.identity = NativeIdentity::new(digest(name.as_bytes()))
        .qualified("kind", MSSQL_RESOURCE_GOVERNOR_COUNTER_KIND)
        .qualified("native_object", "sys.dm_os_performance_counters")
        .qualified("resource_kind", "workload-group")
        .qualified("resource_governor_instance_id", instance)
        .qualified("resource_governor_instance_epoch", epoch);
    governed.counters.remove(0);
    governed.counters[0].native_object = "SQLServer:Workload Group Stats".into();
    governed.counters[0].native_name = "Active requests".into();
    governed.counters[0].native_instance = Some(name.into());
    governed.properties.extend([
        (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), MSSQL_RESOURCE_GOVERNOR_COUNTER_PROFILE.into()),
        (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "1".into()),
        ("resource_name".into(), name.into()),
        ("group_id".into(), "3".into()),
        ("pool_id".into(), "2".into()),
    ]);
    let coverage = StateSourceCoverage::collected(culprit_core::source_id::PERFORMANCE_COUNTERS.into(), 1);
    for set in [marked, governed] {
        let row = OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, set).unwrap();
        let interpret = |owner: &NativeReadContext| {
            explain(PerformanceCounterEvidence::Captured {
                context: owner,
                coverage: &coverage,
                observations: &[&row],
                read_complete: true,
            })
        };
        let report = interpret(&context).unwrap();
        assert!(report.database.is_none() && report.native_database_id.is_none());
        for cause in ["member", "process", "clock", "database"] {
            let mut other = context.clone();
            match cause {
                "member" => {
                    let mut member = member.clone();
                    member.native.id = "other-native-instance".into();
                    other.target = CollectionTarget::Shared(member.clone());
                    let SourcePartition::InstanceLocal { member: slot, .. } = &mut other.partition else { panic!() };
                    *slot = Some(member);
                }
                "process" => {
                    let SourcePartition::InstanceLocal { epoch, .. } = &mut other.partition else { panic!() };
                    *epoch = Some(digest(b"other-process"));
                }
                "clock" => other.observed_until = "2020-01-01T12:00:00Z".into(),
                _ => {
                    other.target =
                        culprit_core::DatabaseIdentity::server(Engine::Mssql, name, "fixture.invalid", 1433, None)
                            .into();
                    other.partition = SourcePartition::Target;
                }
            }
            assert!(interpret(&other).is_err(), "{cause}");
        }
    }
}

fn sample() -> NativeCounterSet {
    let counter = |name: &str, value: &str| NativeCounterSample {
        key: name.into(),
        value: MeasurementValue::Integer(value.into()),
        unit: if name == "Memory Grants Pending" { CounterUnit::Count } else { CounterUnit::Kibibytes },
        nature: CounterNature::Gauge,
        scope: MeasurementScope::Server,
        reset: ResetMarker::Unknown,
        native_object: "SQLServer:Memory Manager".into(),
        native_name: name.into(),
        native_instance: None,
        native_type: Some("65792".into()),
    };
    NativeCounterSet {
        identity: NativeIdentity::new("sys.dm_os_performance_counters")
            .qualified("native_object", "sys.dm_os_performance_counters"),
        scope: MeasurementScope::Server,
        counters: vec![
            counter("Granted Workspace Memory (KB)", "9007199254740993"),
            counter("Memory Grants Pending", "0"),
        ],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: BTreeMap::from([
            (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), PERFORMANCE_COUNTER_PROFILE.into()),
            (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "2".into()),
            (PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:00.500000000Z".into()),
        ]),
    }
}
fn outcome(state: SourceState) -> SourceOutcome {
    SourceOutcome {
        source: culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
        state,
        reason: None,
        reason_message: None,
    }
}
fn explain_sample(set: &NativeCounterSet) -> Result<PerformanceCounterReport> {
    explain(PerformanceCounterEvidence::Snapshot {
        engine: Engine::Mssql,
        samples: &[set],
        source: Some(&outcome(SourceState::Collected)),
    })
}

#[test]
fn exact_counts_zero_native_units_and_missing_fields_remain_independent_shared_context() {
    let report = explain_sample(&sample()).unwrap();
    assert_eq!(report.entries[0].metric.text.as_deref(), Some("9007199254740993 KiB"));
    assert_eq!(report.entries[1].metric.text.as_deref(), Some("0"));
    assert_eq!(report.entries[0].scope, MeasurementScope::Server);
    assert_eq!(report.native_counter_rows, Some(2));
    assert_eq!(report.native_observations, None);
    assert_eq!(report.read_complete, None);
    assert!(
        report.missing_fields.iter().any(|metric| metric.key == "view.performance_counters.field.maximum_workspace")
    );
    assert!(
        report
            .missing_fields
            .iter()
            .all(|metric| metric.text.as_deref() == Some(t!("view.performance_counters.unknown").as_str()))
    );
    assert!(report.limitations.contains(&t!("view.performance_counters.limit.cause")));
}

#[test]
fn unknown_native_type_retains_integer_but_never_inherits_unit_or_lifetime_from_the_name() {
    let mut set = sample();
    set.counters[0].native_type = Some("42".into());
    set.counters[0].nature = CounterNature::Unknown;
    set.counters[0].unit = CounterUnit::Unknown;
    let report = explain_sample(&set).unwrap();
    assert_eq!(report.entries[0].availability, PerformanceCounterAvailability::UnknownNativeType);
    assert_eq!(report.entries[0].raw_value, "9007199254740993");
    assert_eq!(report.entries[0].explanation, t!("view.performance_counters.limit.native_type"));
}

#[test]
fn native_partial_source_and_missing_warehouse_body_keep_original_counts_and_no_healthy_empty() {
    let context = NativeReadContext {
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, None).into(),
        partition: SourcePartition::Target,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        database_writable: None,
    };
    let set = sample();
    let row = OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, set).unwrap();
    let partial = StateSourceCoverage::observed(culprit_core::source_id::PERFORMANCE_COUNTERS.into(), 1, false);
    let report = explain(PerformanceCounterEvidence::Captured {
        context: &context,
        coverage: &partial,
        observations: &[&row],
        read_complete: true,
    })
    .unwrap();
    assert_eq!(report.native_observations, Some(1));
    assert_eq!(report.observed_counter_rows, 2);
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.read_complete, Some(true));
    let coverage = StateSourceCoverage::collected(culprit_core::source_id::PERFORMANCE_COUNTERS.into(), 1);
    let missing = explain(PerformanceCounterEvidence::Captured {
        context: &context,
        coverage: &coverage,
        observations: &[],
        read_complete: false,
    })
    .unwrap();
    assert_eq!(missing.native_observations, Some(1));
    assert_eq!(missing.native_counter_rows, None);
    assert_eq!(missing.observed_counter_rows, 0);
    assert!(missing.entries.is_empty());
    assert_eq!(missing.source_state, Some(SourceState::Partial));
    let mut future = row.clone();
    let OperationalFact::CounterSet(set) = &mut future.fact else { panic!() };
    set.properties.insert(PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:02.000000000Z".into());
    assert!(
        explain(PerformanceCounterEvidence::Captured {
            context: &context,
            coverage: &coverage,
            observations: &[&future],
            read_complete: true
        })
        .is_err()
    );
}

#[test]
fn malformed_scope_type_integer_roster_clock_and_duplicate_native_rows_fail_before_rendering() {
    for cause in ["scope", "float", "type", "rows", "clock", "duplicate", "bigint"] {
        let mut set = sample();
        match cause {
            "scope" => {
                set.scope = MeasurementScope::Database;
                for counter in &mut set.counters {
                    counter.scope = MeasurementScope::Database;
                }
            }
            "float" => set.counters[0].value = MeasurementValue::Real(1.0),
            "type" => set.counters[0].native_type = Some("42".into()),
            "rows" => {
                set.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "3".into());
            }
            "clock" => {
                set.properties.insert(PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:00.5Z".into());
            }
            "bigint" => set.counters[0].value = MeasurementValue::Integer(u64::MAX.to_string()),
            _ => {
                let mut row = set.counters[0].clone();
                row.key = "different-key-same-native-row".into();
                set.counters.push(row);
                set.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "3".into());
            }
        }
        assert!(set.is_valid(), "generic original remains representable: {cause}");
        assert!(explain_sample(&set).is_err(), "{cause}");
    }
}

#[test]
fn old_missing_selection_metadata_keeps_positive_facts_without_inventing_a_requested_roster() {
    let mut set = sample();
    set.properties.clear();
    let report = explain_sample(&set).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert!(report.missing_fields.is_empty());
    assert_eq!(report.native_counter_rows, None);
    assert_eq!(report.entries[0].observed_at, None);
    assert!(report.limitations.contains(&t!("view.performance_counters.limit.selection")));
    assert!(report.limitations.contains(&t!("view.performance_counters.limit.clock")));
}

#[test]
fn snapshot_sharing_keeps_a_repeatable_withheld_explanation_without_repairing_invalid_semantics() {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.sources = vec![outcome(SourceState::Collected)];
    let mut set = sample();
    set.properties.insert("credential_canary".into(), "password=secret_canary /private/canary".into());
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![set]);
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    let report = explain_snapshot(&shared).unwrap().unwrap();
    assert!(report.fields_withheld);
    assert!(report.entries.is_empty());
    assert_eq!(report.observed_counter_rows, 2);
    assert!(!serde_json::to_string(&shared).unwrap().contains("canary"));
    let again = culprit_core::redact::redact_snapshot(&shared);
    assert!(explain_snapshot(&again).unwrap().unwrap().fields_withheld);
    let set = &mut snapshot.pressure.as_mut().unwrap().resource_counters.as_mut().unwrap()[0];
    set.counters[0].unit = CounterUnit::Bytes;
    let refused = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(refused.sources.iter().any(|source| source.source == culprit_core::source_id::PERFORMANCE_COUNTERS
        && source.state == SourceState::Unavailable));
    assert!(explain_snapshot(&refused).unwrap().unwrap().entries.is_empty());
}

#[test]
fn scalar_descriptor_withholding_preserves_existing_same_read_fraction_relationships() {
    let mut set = sample();
    for (name, value, nature, code) in [
        ("Cache Hit Ratio", "1", CounterNature::FractionNumerator, "537003264"),
        ("Cache Hit Ratio Base", "2", CounterNature::FractionBase, "1073939712"),
    ] {
        set.counters.push(NativeCounterSample {
            key: name.into(),
            value: MeasurementValue::Integer(value.into()),
            unit: CounterUnit::Count,
            nature,
            scope: MeasurementScope::Server,
            reset: ResetMarker::Unknown,
            native_object: "SQLServer:Plan Cache".into(),
            native_name: name.into(),
            native_instance: Some("SQL Plans".into()),
            native_type: Some(code.into()),
        });
    }
    set.ratios.push(NativeCounterRatio {
        kind: CounterRatioKind::PlanCacheHit,
        numerator: Some("Cache Hit Ratio".into()),
        base: Some("Cache Hit Ratio Base".into()),
        support: CounterRatioSupport::Supported,
        observed_at: set.properties.get(PERFORMANCE_COUNTER_CLOCK_PROPERTY).cloned(),
    });
    set.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "4".into());
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.sources = vec![outcome(SourceState::Collected)];
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![set]);
    let original = explain_snapshot(&snapshot).unwrap().unwrap();
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    let report = explain_snapshot(&shared).unwrap().unwrap();
    assert!(report.fields_withheld);
    assert!(report.entries.is_empty());
    let expected = &original.ratios.as_ref().unwrap().entries[0].metrics;
    let actual = &report.ratios.as_ref().unwrap().entries[0].metrics;
    assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(expected).unwrap());
    assert!(actual.iter().any(|metric| metric.key == "view.counter_ratios.metric.percent"));
    let again = culprit_core::redact::redact_snapshot(&shared);
    assert_eq!(
        serde_json::to_value(explain_snapshot(&again).unwrap().unwrap().ratios.unwrap().entries[0].metrics.clone())
            .unwrap(),
        serde_json::to_value(actual).unwrap()
    );
}

#[test]
fn localized_reports_reword_labels_without_changing_native_identity_value_unit_or_clock() {
    let set = sample();
    let english = culprit_core::i18n::scope("en", || explain_sample(&set).unwrap());
    for code in ["es", "fr", "de"] {
        let report = culprit_core::i18n::scope(code, || explain_sample(&set).unwrap());
        assert_ne!(report.title, english.title);
        assert_ne!(report.entries[0].metric.label, english.entries[0].metric.label);
        assert_eq!(report.entries[0].raw_value, english.entries[0].raw_value);
        assert_eq!(report.entries[0].native_type, english.entries[0].native_type);
        assert_eq!(report.entries[0].unit, english.entries[0].unit);
        assert_eq!(report.entries[0].observed_at, english.entries[0].observed_at);
    }
}

fn database_evidence() -> (CollectionDescription, OperationalObservation) {
    let native = NativeIdentity::new("7")
        .qualified("kind", "sql-server-local-database-recovery-epoch")
        .qualified("database_created_without_offset", "2019-01-01T00:00:00.0000000")
        .qualified("database_file_guid", "file")
        .qualified("database_guid", "database")
        .qualified("recovery_fork_guid", "fork")
        .qualified("instance_epoch", "process");
    let mut set = sample();
    set.identity = native.clone();
    set.scope = MeasurementScope::Database;
    set.counters.truncate(1);
    let counter = &mut set.counters[0];
    counter.scope = MeasurementScope::Database;
    counter.native_object = "SQLServer:Databases".into();
    counter.native_name = "Log Bytes Flushed/sec".into();
    counter.native_instance = Some("app".into());
    counter.native_type = Some("272696576".into());
    counter.nature = CounterNature::Cumulative;
    counter.unit = CounterUnit::Bytes;
    set.properties.insert(PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), DATABASE_PERFORMANCE_COUNTER_PROFILE.into());
    set.properties.insert(DATABASE_PERFORMANCE_NAME_PROPERTY.into(), "app".into());
    set.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "1".into());
    set.properties.insert("catalog_visibility".into(), "complete".into());
    set.properties.insert("catalog_name_matches".into(), "1".into());
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("host"),
                native: NativeIdentity::new("instance"),
            }),
            epoch: Some(native.key().unwrap()),
        },
        source: "database-performance-counter-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::DatabasePerformanceCounters {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::DatabasePerformanceCounters,
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(
                    culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
                    1,
                    false,
                )],
            },
            native_database: Box::new(native),
            database_writable: true,
        },
    };
    let row = OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, set).unwrap();
    (source, row)
}
#[test]
fn selected_database_report_keeps_only_its_three_fields_and_never_infers_a_rate_or_shared_population() {
    let (source, row) = database_evidence();
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    let report = explain(PerformanceCounterEvidence::Database {
        source: &source,
        coverage,
        observations: &[&row],
        read_complete: true,
    })
    .unwrap();
    assert_eq!(report.database.as_deref(), Some("app"));
    assert_eq!(report.native_database_id.as_deref(), Some("7"));
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].field, "log_bytes_flushed");
    assert_eq!(report.entries[0].raw_value, "9007199254740993");
    assert_eq!(report.entries[0].metric.text.as_deref(), Some("9007199254740993 B"));
    assert_eq!(report.missing_fields.len(), 2);
    assert!(report.missing_fields.iter().all(|field| {
        ["view.performance_counters.field.log_flushes", "view.performance_counters.field.log_flush_wait"]
            .contains(&field.key.as_str())
    }));
    assert!(report.ratios.is_none());
    assert!(report.limitations.contains(&t!("view.performance_counters.limit.database")));
    assert!(report.limitations.contains(&t!("view.performance_counters.limit.lifetime")));
    assert_eq!(report.source_state, Some(SourceState::Partial));
}
#[test]
fn selected_report_refuses_foreign_source_counter_clock_or_native_database_before_wording() {
    for cause in ["scope", "source", "epoch", "clock", "generic"] {
        let (mut source, mut row) = database_evidence();
        match cause {
            "scope" => {
                let OperationalFact::CounterSet(set) = &mut row.fact else { panic!() };
                set.counters[0].native_instance = Some("_Total".into());
            }
            "source" => row.source = "unrelated".into(),
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                *epoch = Some("foreign".into());
            }
            "clock" => {
                let OperationalFact::CounterSet(set) = &mut row.fact else { panic!() };
                set.properties
                    .insert(PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:01.000000001Z".into());
            }
            _ => source.observation = SourceObservation::Snapshot(source.observation.snapshot().unwrap().clone()),
        }
        let coverage = &source.observation.snapshot().unwrap().sources[0];
        assert!(
            explain(PerformanceCounterEvidence::Database {
                source: &source,
                coverage,
                observations: &[&row],
                read_complete: true
            })
            .is_err(),
            "{cause}"
        );
    }
}

mod recorded;

mod detailed;
