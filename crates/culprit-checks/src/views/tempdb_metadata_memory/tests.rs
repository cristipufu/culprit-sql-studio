use super::*;
use culprit_core::{Engine, MeasurementScope};
use std::collections::BTreeMap;

fn context() -> NativeReadContext {
    let owner = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-machine"),
        native: NativeIdentity::new("native-instance"),
    };
    NativeReadContext {
        target: CollectionTarget::Shared(owner.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(owner), epoch: Some("native-process-epoch".into()) },
        observed_from: "2020-01-01T06:02:00Z".into(),
        observed_until: "2020-01-01T06:02:01Z".into(),
        database_writable: None,
    }
}

fn identity(id: &str, kind: &str, object: &str) -> NativeIdentity {
    NativeIdentity::new(id)
        .qualified("kind", kind)
        .qualified("native_object", object)
        .qualified("database_id", "2")
        .qualified("database_created", "2020-01-01T08:00:00.003")
}

fn feature(enabled: Option<bool>) -> NativeCounterSet {
    let mut properties = BTreeMap::from([
        ("native_major".into(), "16".into()),
        ("native_edition".into(), "3".into()),
        ("observed_at".into(), "2020-01-01T06:02:00Z".into()),
    ]);
    if let Some(enabled) = enabled {
        properties.insert("metadata_enabled".into(), i32::from(enabled).to_string());
    } else {
        properties.insert("metadata_enabled_state".into(), "native-null".into());
    }
    NativeCounterSet {
        identity: identity("feature", MSSQL_TEMPDB_METADATA_FEATURE_KIND, "SERVERPROPERTY")
            .qualified("native_property", "IsTempDbMetadataMemoryOptimized"),
        scope: MeasurementScope::Server,
        counters: vec![],
        ratios: vec![],
        ratio_gaps: vec![],
        properties,
    }
}

fn consumer(allocated: Option<u64>, used: Option<u64>) -> NativeCounterSet {
    let mut properties = BTreeMap::from([
        ("native_major".into(), "16".into()),
        ("native_edition".into(), "3".into()),
        ("observed_at".into(), "2020-01-01T06:02:00.5Z".into()),
        ("metadata_enabled".into(), "1".into()),
        ("memory_consumer_type".into(), "2".into()),
        ("memory_consumer_type_desc".into(), "VARHEAP".into()),
        ("memory_consumer_desc".into(), "Transaction".into()),
    ]);
    properties.insert("used_bytes_state".into(), if used.is_some() { "measured" } else { "native-null" }.into());
    let counters = [("allocated_bytes", allocated), ("used_bytes", used)]
        .into_iter()
        .filter_map(|(field, value)| {
            value.map(|value| {
                NativeCounterSample::gauge(
                    field,
                    value,
                    CounterUnit::Bytes,
                    MeasurementScope::Server,
                    MSSQL_TEMPDB_METADATA_CONSUMERS,
                    field,
                )
            })
        })
        .collect();
    NativeCounterSet {
        identity: identity("42", MSSQL_TEMPDB_METADATA_CONSUMER_KIND, MSSQL_TEMPDB_METADATA_CONSUMERS),
        scope: MeasurementScope::Server,
        counters,
        ratios: vec![],
        ratio_gaps: vec![],
        properties,
    }
}

fn rows(samples: Vec<NativeCounterSet>) -> Vec<OperationalObservation> {
    samples
        .into_iter()
        .map(|sample| {
            OperationalObservation::from_counters(culprit_core::source_id::TEMPDB_METADATA_MEMORY, sample).unwrap()
        })
        .collect()
}

fn coverage(records: u32, complete: bool) -> StateSourceCoverage {
    StateSourceCoverage::observed(culprit_core::source_id::TEMPDB_METADATA_MEMORY.into(), records, complete)
}

fn report(rows: &[OperationalObservation], complete: bool) -> TempdbMetadataMemoryReport {
    explain(TempdbMetadataMemoryEvidence {
        context: &context(),
        coverage: &coverage(rows.len() as u32, complete),
        observations: &rows.iter().collect::<Vec<_>>(),
        read_complete: true,
    })
    .unwrap()
}

#[test]
fn actual_feature_and_consumer_points_distinguish_reservation_usage_and_zero_without_a_pressure_or_owner_verdict() {
    for (allocated, used, key) in [
        (8192, 4096, "view.tempdb_metadata_memory.usage.reservation"),
        (8192, 8192, "view.tempdb_metadata_memory.usage.used"),
        (0, 0, "view.tempdb_metadata_memory.usage.zero"),
        (1, 2, "view.tempdb_metadata_memory.usage.inconsistent"),
        (9007199254740993, 9007199254740992, "view.tempdb_metadata_memory.usage.reservation"),
    ] {
        let rows = rows(vec![feature(Some(true)), consumer(Some(allocated), Some(used))]);
        let original = serde_json::to_value(&rows).unwrap();
        let report = report(&rows, true);
        assert_eq!(report.metadata_enabled, Some(true));
        assert_eq!(report.feature_observed_at.as_deref(), Some("2020-01-01T06:02:00Z"));
        assert_eq!(report.entries[0].observed_at, "2020-01-01T06:02:00.5Z");
        assert_eq!(report.entries[0].allocated_bytes, Some(allocated.to_string()));
        assert_eq!(report.entries[0].used_bytes, Some(used.to_string()));
        assert_eq!(report.entries[0].explanation, culprit_core::i18n::t(key, &[]));
        assert_eq!(report.entries[0].observation_key, rows[1].key);
        assert_eq!(serde_json::to_value(&rows).unwrap(), original);
        let json = serde_json::to_value(&report).unwrap();
        for absent in ["severity", "cause", "fix_sql", "free_bytes", "reclaimable_bytes", "rate"] {
            assert!(json.get(absent).is_none());
            assert!(json["entries"][0].get(absent).is_none());
        }
    }
}

#[test]
fn native_feature_off_null_and_missing_consumer_data_remain_distinct_without_a_healthy_zero() {
    let off = report(&rows(vec![feature(Some(false))]), true);
    assert_eq!(off.metadata_enabled, Some(false));
    assert!(off.entries.is_empty());
    assert_eq!(off.summary, t!("view.tempdb_metadata_memory.summary.disabled"));
    let unknown = report(&rows(vec![feature(None)]), false);
    assert_eq!(unknown.metadata_enabled, None);
    assert_eq!(unknown.source_state, SourceState::Partial);
    assert_eq!(unknown.summary, t!("view.tempdb_metadata_memory.summary.unavailable"));
    let no_consumers = report(&rows(vec![feature(Some(true))]), true);
    assert_eq!(no_consumers.metadata_enabled, Some(true));
    assert_eq!(no_consumers.summary, t!("view.tempdb_metadata_memory.summary.unavailable"));
    assert!(no_consumers.entries.is_empty());
    let empty = report(&[], true);
    assert!(empty.entries.is_empty() && empty.metadata_enabled.is_none());
    assert_eq!(empty.summary, t!("view.tempdb_metadata_memory.summary.unavailable"));
}

#[test]
fn native_null_and_unsupported_usage_semantics_preserve_known_reservation_without_inventing_used_bytes() {
    for (allocated, used) in [(None, Some(4096)), (Some(8192), None)] {
        let report = report(&rows(vec![feature(Some(true)), consumer(allocated, used)]), true);
        assert_eq!(report.entries[0].allocated_bytes, allocated.map(|value| value.to_string()));
        assert_eq!(report.entries[0].used_bytes, used.map(|value| value.to_string()));
        assert_eq!(report.entries[0].explanation, t!("view.tempdb_metadata_memory.usage.unknown"));
    }
    for (code, name) in [(4, "PGPOOL"), (9, "FUTURE")] {
        let mut sample = consumer(Some(8192), None);
        sample.properties.insert("memory_consumer_type".into(), code.to_string());
        sample.properties.insert("memory_consumer_type_desc".into(), name.into());
        sample.properties.insert("used_bytes_state".into(), "not-applicable".into());
        sample.properties.insert("native_used_bytes".into(), "4096".into());
        let rows = rows(vec![feature(Some(true)), sample]);
        let report = report(&rows, true);
        assert_eq!(report.entries[0].allocated_bytes.as_deref(), Some("8192"));
        assert_eq!(report.entries[0].used_bytes, None);
        assert_eq!(report.entries[0].native_type_code, code.to_string());
        assert_eq!(report.entries[0].explanation, t!("view.tempdb_metadata_memory.usage.not_applicable"));
        assert!(serde_json::to_string(&rows).unwrap().contains("native_used_bytes"));
    }
}

#[test]
fn partial_source_or_archive_visibility_retains_positive_consumers_and_independent_counts() {
    let rows = rows(vec![feature(Some(true)), consumer(Some(8192), Some(4096))]);
    let partial = report(&rows, false);
    assert_eq!(partial.source_state, SourceState::Partial);
    assert_eq!(partial.entries.len(), 1);
    assert!(partial.limitations.contains(&t!("view.tempdb_metadata_memory.limit.partial")));
    let source = coverage(2, true);
    let captured = explain(TempdbMetadataMemoryEvidence {
        context: &context(),
        coverage: &source,
        observations: &[&rows[1]],
        read_complete: false,
    })
    .unwrap();
    assert_eq!(captured.metadata_enabled, Some(true), "the actual consumer also reports native feature state");
    assert!(captured.feature_observed_at.is_none());
    assert_eq!((captured.native_rows, captured.observed_rows, captured.entries.len()), (2, 1, 1));
    assert_eq!(captured.source_state, SourceState::Partial);
    assert!(
        explain(TempdbMetadataMemoryEvidence {
            context: &context(),
            coverage: &coverage(1, true),
            observations: &[&rows[1]],
            read_complete: true
        })
        .is_err(),
        "a claimed full body cannot omit its native profile"
    );
}

#[test]
fn source_failures_explain_missing_consumers_without_erasing_an_independent_native_feature_point() {
    let native = rows(vec![feature(Some(true))]);
    for (error, expected) in [
        (ErrorCode::PermissionDenied, t!("view.tempdb_metadata_memory.source.denied")),
        (ErrorCode::Unsupported, t!("view.tempdb_metadata_memory.source.unsupported")),
        (ErrorCode::QueryFailed, t!("view.tempdb_metadata_memory.source.failed")),
    ] {
        for retained_feature in [false, true] {
            let mut source = if retained_feature {
                coverage(1, false)
            } else {
                StateSourceCoverage::unavailable(culprit_core::source_id::TEMPDB_METADATA_MEMORY.into(), error, None)
            };
            source.error_code = Some(error);
            let rows = if retained_feature { native.iter().collect::<Vec<_>>() } else { vec![] };
            let report = explain(TempdbMetadataMemoryEvidence {
                context: &context(),
                coverage: &source,
                observations: &rows,
                read_complete: true,
            })
            .unwrap();
            assert!(report.limitations.contains(&expected));
            assert_eq!(report.native_source, source);
            assert_eq!(report.metadata_enabled, retained_feature.then_some(true));
            assert!(report.entries.is_empty(), "failed visibility cannot become measured zero consumer usage");
        }
    }
}

#[test]
fn a_recorded_stop_keeps_its_specific_lifecycle_reason_instead_of_a_generic_read_error() {
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let error = stop.error();
        let source = StateSourceCoverage::unavailable(
            culprit_core::source_id::TEMPDB_METADATA_MEMORY.into(),
            error.code,
            Some(stop),
        );
        let report = explain(TempdbMetadataMemoryEvidence {
            context: &context(),
            coverage: &source,
            observations: &[],
            read_complete: true,
        })
        .unwrap();
        assert!(report.limitations.contains(&error.message));
        assert!(!report.limitations.contains(&t!("view.tempdb_metadata_memory.source.failed")));
        assert_eq!(report.native_source.stopped, Some(stop));
        assert_eq!(report.metadata_enabled, None);
        assert!(report.entries.is_empty());
    }
}

#[test]
fn database_routes_foreign_members_and_unknown_epochs_cannot_acquire_shared_tempdb_authority() {
    let rows = rows(vec![feature(Some(true)), consumer(Some(8192), Some(4096))]);
    for missing in ["target", "member", "epoch", "foreign-member", "engine"] {
        let mut context = context();
        match missing {
            "target" => {
                context.target =
                    culprit_core::DatabaseIdentity::server(Engine::Mssql, "tempdb", "native.invalid", 1433, None).into()
            }
            "member" => {
                context.partition =
                    SourcePartition::InstanceLocal { member: None, epoch: Some("native-process-epoch".into()) }
            }
            "epoch" => context.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "foreign-member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else {
                    panic!()
                };
                member.native.id = "other-instance".into();
            }
            "engine" => {
                let CollectionTarget::Shared(owner) = &mut context.target else { panic!() };
                owner.engine = Engine::Mysql;
            }
            _ => unreachable!(),
        }
        assert!(
            explain(TempdbMetadataMemoryEvidence {
                context: &context,
                coverage: &coverage(2, true),
                observations: &rows.iter().collect::<Vec<_>>(),
                read_complete: true
            })
            .is_err(),
            "{missing}"
        );
    }
}

#[test]
fn native_source_clock_feature_and_tempdb_generation_contradictions_are_not_repaired_by_later_consumers() {
    for changed in [
        "clock-before",
        "clock-after",
        "clock-local",
        "creation",
        "database",
        "major",
        "edition",
        "aggregate",
        "feature",
        "unit",
        "nature",
    ] {
        let mut sample = consumer(Some(8192), Some(4096));
        match changed {
            "clock-before" => {
                sample.properties.insert("observed_at".into(), "2020-01-01T06:01:59Z".into());
            }
            "clock-after" => {
                sample.properties.insert("observed_at".into(), "2020-01-01T06:02:02Z".into());
            }
            "clock-local" => {
                sample.properties.insert("observed_at".into(), "2020-01-01T06:02:00.5".into());
            }
            "creation" => {
                sample.identity.qualifiers.insert("database_created".into(), "2020-01-02T08:00:00.003".into());
            }
            "database" => {
                sample.identity.qualifiers.insert("database_id".into(), "7".into());
            }
            "major" => {
                sample.properties.insert("native_major".into(), "15".into());
            }
            "edition" => {
                sample.properties.insert("native_edition".into(), "8".into());
            }
            "aggregate" => {
                sample.properties.insert("memory_consumer_type".into(), "0".into());
            }
            "feature" => {
                sample.properties.insert("metadata_enabled".into(), "0".into());
            }
            "unit" => sample.counters[0].unit = CounterUnit::Kibibytes,
            "nature" => sample.counters[0].nature = CounterNature::Cumulative,
            _ => unreachable!(),
        }
        let rows = rows(vec![feature(Some(true)), sample]);
        assert!(
            explain(TempdbMetadataMemoryEvidence {
                context: &context(),
                coverage: &coverage(2, true),
                observations: &rows.iter().collect::<Vec<_>>(),
                read_complete: true
            })
            .is_err(),
            "{changed}"
        );
    }
    for state in [Some(false), None] {
        let rows = rows(vec![feature(state), consumer(Some(8192), Some(4096))]);
        assert!(
            explain(TempdbMetadataMemoryEvidence {
                context: &context(),
                coverage: &coverage(2, true),
                observations: &rows.iter().collect::<Vec<_>>(),
                read_complete: true
            })
            .is_err()
        );
    }
}

#[test]
fn duplicate_ids_mixed_native_points_and_wrong_source_families_withdraw_the_interpretation_without_rewriting_raw_rows()
{
    for changed in ["duplicate", "point", "source"] {
        let mut other = consumer(Some(8192), Some(4096));
        if changed != "duplicate" {
            other.identity.id = "43".into();
        }
        if changed == "point" {
            other.properties.insert("observed_at".into(), "2020-01-01T06:02:00.6Z".into());
        }
        let mut rows = rows(vec![feature(Some(true)), consumer(Some(8192), Some(4096)), other]);
        let mut source = coverage(3, true);
        if changed == "source" {
            source.source = culprit_core::source_id::MEMORY_CLERKS.into();
            for row in &mut rows {
                row.source = source.source.clone();
            }
        }
        let original = serde_json::to_value(&rows).unwrap();
        assert!(
            explain(TempdbMetadataMemoryEvidence {
                context: &context(),
                coverage: &source,
                observations: &rows.iter().collect::<Vec<_>>(),
                read_complete: true
            })
            .is_err(),
            "{changed}"
        );
        assert_eq!(serde_json::to_value(&rows).unwrap(), original);
    }
}
