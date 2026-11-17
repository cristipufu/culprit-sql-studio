use super::*;
use crate::{DatabaseIdentity, Engine, SourceState};

fn snapshot() -> ConnectionOutcomeSnapshot {
    let counters = NativeCounterSet {
        identity: NativeIdentity::new("pg_stat_database")
            .qualified("dbid", "42")
            .qualified("postmaster_started_at", "2020-01-01T23:00:00Z"),
        scope: MeasurementScope::Database,
        properties: BTreeMap::from([
            ("datname".into(), "fixture".into()),
            ("server_version_num".into(), "180000".into()),
            ("counter_observed_at".into(), "2020-01-02T00:00:00Z".into()),
        ]),
        counters: vec![NativeCounterSample {
            key: "sessions".into(),
            value: MeasurementValue::Integer("9007199254740993".into()),
            unit: CounterUnit::Count,
            nature: CounterNature::Cumulative,
            scope: MeasurementScope::Database,
            reset: ResetMarker::At("2020-01-01T00:00:00Z".into()),
            native_object: "pg_stat_database".into(),
            native_name: "sessions".into(),
            native_instance: None,
            native_type: None,
        }],
    };
    ConnectionOutcomeSnapshot {
        target: DatabaseIdentity::server(Engine::Postgres, "fixture", "fixture.invalid", 5432, None).into(),
        context: ObservationReadContext::Collector {
            observed_from: "2026-01-01T00:00:00Z".into(),
            observed_until: "2026-01-01T00:00:01Z".into(),
        },
        sources: vec![StateSourceCoverage::observed("pg_stat_database".into(), 1, true)],
        observations: vec![ConnectionOutcomeObservation { source: "pg_stat_database".into(), counters }],
    }
}

#[test]
fn native_cumulative_count_uses_only_its_own_reset_and_clock_not_client_or_process_age() {
    let read = snapshot();
    assert!(read.is_valid());
    let rate = read.rate(ConnectionOutcomeMetricKind::SessionsEstablished).unwrap();
    assert_eq!(rate.seconds, 86400.0);
    assert_eq!(rate.per_second, 9007199254740993u64 as f64 / 86400.0);
    for reset in [
        ResetMarker::Unknown,
        ResetMarker::Generation("same-process".into()),
        ResetMarker::At("2020-01-02T00:00:01Z".into()),
        ResetMarker::At("2020-01-01T23:30:00Z".into()),
    ] {
        let mut invalid = read.clone();
        invalid.observations[0].counters.counters[0].reset = reset;
        assert!(invalid.rate(ConnectionOutcomeMetricKind::SessionsEstablished).is_none());
        assert_eq!(invalid.observations[0].counters.counters[0].value.integer(), Some(9007199254740993));
    }
}

#[test]
fn public_rates_reject_invalid_header_wrong_scope_foreign_database_and_duplicate_populations() {
    for mutation in ["count", "scope", "database", "state", "duplicate", "clock", "unzoned-clock"] {
        let mut read = snapshot();
        match mutation {
            "count" => read.sources[0].records = 0,
            "scope" => {
                read.observations[0].counters.scope = MeasurementScope::Server;
                read.observations[0].counters.counters[0].scope = MeasurementScope::Server;
            }
            "database" => {
                read.observations[0].counters.properties.insert("datname".into(), "other".into());
            }
            "state" => read.sources[0].state = SourceState::Unavailable,
            "duplicate" => {
                read.observations.push(read.observations[0].clone());
                read.sources[0].records = 2;
            }
            "clock" => {
                read.observations[0].counters.properties.insert("counter_observed_at".into(), "invalid".into());
            }
            "unzoned-clock" => {
                read.observations[0]
                    .counters
                    .properties
                    .insert("counter_observed_at".into(), "2020-01-02T00:00:00".into());
            }
            _ => unreachable!(),
        }
        assert!(!read.is_valid(), "{mutation}");
        assert!(read.rate(ConnectionOutcomeMetricKind::SessionsEstablished).is_none(), "{mutation}");
    }
}

#[test]
fn native_clock_requires_original_native_bounds_but_never_client_clock_containment() {
    let mut read = snapshot();
    assert!(read.is_valid(), "independent client and server clocks");
    read.context = ObservationReadContext::Native {
        context: Box::new(NativeReadContext {
            target: read.target.clone(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T23:59:59Z".into(),
            observed_until: "2020-01-02T00:00:01Z".into(),
            database_writable: None,
        }),
    };
    assert!(read.is_valid());
    let ObservationReadContext::Native { context } = &mut read.context else { unreachable!() };
    context.observed_until = "2020-01-01T23:59:59Z".into();
    assert!(!read.is_valid());
    assert!(read.rate(ConnectionOutcomeMetricKind::SessionsEstablished).is_none());
}

#[test]
fn complete_and_partial_decodes_preserve_original_native_header_and_unmapped_rows() {
    let mut read = snapshot();
    read.sources[0].records = 2;
    assert!(!read.is_valid());
    assert!(read.is_valid_read(false));
    assert_eq!(read.sources[0].records, 2);
    let mut unrelated = read.observations[0].clone();
    unrelated.counters.identity = NativeIdentity::new("another-native-variable");
    read.observations.push(unrelated);
    assert!(read.is_valid());
    assert!(read.profile(&read.observations[1]).is_none());
    assert_eq!(read.sources[0].records, 2);
}

#[test]
fn metric_vocabulary_is_explicit_for_every_engine_and_unknown_sources_never_borrow_it() {
    let read = snapshot();
    let native = &read.observations[0].counters;
    for engine in Engine::ALL {
        assert!(engine.spec().connection_outcome_profile("unknown-native-source", native).is_none());
        if engine != Engine::Postgres {
            assert!(engine.spec().connection_outcome_profile("pg_stat_database", native).is_none());
        }
    }
}

#[test]
fn a_saved_point_needs_the_same_outer_database_namespace_and_known_identity_without_shared_substitution() {
    let mut read = snapshot();
    let mut outer: Box<crate::Snapshot> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/postgres-demo.json")))
            .unwrap();
    outer.server.engine = Engine::Postgres;
    outer.server.database = "fixture".into();
    let CollectionTarget::Database(database) = &mut read.target else { unreachable!() };
    database.incarnation = Some("original-native-database".into());
    outer.bind_identity(database.clone());
    assert!(read.target.snapshot_namespace_comparison(&outer).allows_comparison());
    for change in ["database", "engine", "incarnation", "missing", "unknown"] {
        let mut changed = outer.clone();
        match change {
            "database" => changed.server.database = "another".into(),
            "engine" => changed.server.engine = Engine::Mysql,
            "incarnation" => {
                changed.identity.as_mut().unwrap().incarnation = Some("replacement-native-database".into())
            }
            "missing" => changed.identity = None,
            "unknown" => changed.identity.as_mut().unwrap().location = crate::DatabaseLocation::Unknown,
            _ => unreachable!(),
        }
        assert!(!read.target.snapshot_namespace_comparison(&changed).allows_comparison(), "{change}");
    }
    read.target = CollectionTarget::Shared(SharedResourceIdentity {
        engine: Engine::Postgres,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-system"),
        native: NativeIdentity::new("native-member"),
    });
    assert!(!read.target.snapshot_namespace_comparison(&outer).allows_comparison());
}
