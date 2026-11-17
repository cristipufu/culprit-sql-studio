use super::*;
use culprit_core::{ConnectionOutcomeObservation, DatabaseIdentity, Engine, telemetry::*};
use std::collections::BTreeMap;

pub(crate) fn snapshot(version: u32) -> ConnectionOutcomeSnapshot {
    ConnectionOutcomeSnapshot {
        target: DatabaseIdentity::server(Engine::Postgres, "fixture", "fixture.invalid", 5432, None).into(),
        context: ObservationReadContext::Collector {
            observed_from: "2026-01-01T00:00:00Z".into(),
            observed_until: "2026-01-01T00:00:01Z".into(),
        },
        sources: vec![StateSourceCoverage::observed("pg_stat_database".into(), 1, true)],
        observations: vec![ConnectionOutcomeObservation {
            source: "pg_stat_database".into(),
            counters: NativeCounterSet {
                ratios: Vec::new(),
                ratio_gaps: Vec::new(),
                identity: NativeIdentity::new("pg_stat_database").qualified("dbid", "42"),
                scope: MeasurementScope::Database,
                properties: BTreeMap::from([
                    ("datname".into(), "fixture".into()),
                    ("server_version_num".into(), version.to_string()),
                    ("counter_observed_at".into(), "2020-01-02T00:00:00Z".into()),
                ]),
                counters: vec![
                    count("sessions", "9007199254740993"),
                    count("sessions_abandoned", "0"),
                    count("sessions_killed", "7"),
                    NativeCounterSample {
                        key: "active_time".into(),
                        value: MeasurementValue::Real(0.0),
                        unit: CounterUnit::Milliseconds,
                        nature: CounterNature::Cumulative,
                        scope: MeasurementScope::Database,
                        reset: ResetMarker::Unknown,
                        native_object: "pg_stat_database".into(),
                        native_name: "active_time".into(),
                        native_instance: None,
                        native_type: None,
                    },
                ],
            },
        }],
    }
}
fn count(key: &str, value: &str) -> NativeCounterSample {
    NativeCounterSample {
        key: key.into(),
        value: MeasurementValue::Integer(value.into()),
        unit: CounterUnit::Count,
        nature: CounterNature::Cumulative,
        scope: MeasurementScope::Database,
        reset: ResetMarker::At("2020-01-01T00:00:00Z".into()),
        native_object: "pg_stat_database".into(),
        native_name: key.into(),
        native_instance: None,
        native_type: None,
    }
}

#[test]
fn native_outcomes_keep_exact_values_distinct_reasons_and_no_invented_query_or_session_latency() {
    let raw = snapshot(180000);
    let report = explain(&raw, true).unwrap();
    assert_eq!((report.native_rows, report.observed_rows, report.mapped_rows), (1, 1, 1));
    assert_eq!(report.entries[0].scope, MeasurementScope::Database);
    let metrics = &report.entries[0].metrics;
    let metric = |kind| metrics.iter().find(|metric| metric.kind == kind).unwrap();
    assert_eq!(
        metric(ConnectionOutcomeMetricKind::SessionsEstablished).value,
        Some(MeasurementValue::Integer("9007199254740993".into()))
    );
    assert_eq!(
        metric(ConnectionOutcomeMetricKind::SessionsLostClient).value,
        Some(MeasurementValue::Integer("0".into()))
    );
    assert_eq!(metric(ConnectionOutcomeMetricKind::SessionsFatal).value, None);
    assert_eq!(metric(ConnectionOutcomeMetricKind::SessionsFatal).availability, ConnectionOutcomeAvailability::Unknown);
    assert_eq!(
        metric(ConnectionOutcomeMetricKind::SessionsOperatorTerminated).value,
        Some(MeasurementValue::Integer("7".into()))
    );
    assert_eq!(metric(ConnectionOutcomeMetricKind::ActiveTime).value, Some(MeasurementValue::Real(0.0)));
    assert_eq!(report.entries[0].reference.observation_key, raw.observations[0].key().unwrap());
    assert!(report.limitations.contains(&t!("view.connection_outcomes.limit.owner")));
    assert!(report.limitations.contains(&t!("view.connection_outcomes.limit.current")));
}

#[test]
fn older_native_schema_is_unsupported_while_zero_unknown_and_unknown_reset_remain_separate() {
    let mut legacy = snapshot(130000);
    legacy.observations[0].counters.counters.clear();
    for key in [
        "sessions",
        "sessions_abandoned",
        "sessions_fatal",
        "sessions_killed",
        "session_time",
        "active_time",
        "idle_in_transaction_time",
    ] {
        legacy.observations[0].counters.properties.insert(format!("has_{key}"), "false".into());
    }
    let report = explain(&legacy, true).unwrap();
    assert!(
        report.entries[0]
            .metrics
            .iter()
            .all(|metric| metric.availability == ConnectionOutcomeAvailability::Unsupported && metric.value.is_none())
    );
    let mut current = snapshot(140000);
    for counter in &mut current.observations[0].counters.counters {
        counter.reset = ResetMarker::Unknown;
    }
    let report = explain(&current, true).unwrap();
    assert!(report.entries[0].metrics.iter().all(|metric| metric.rate.is_none()));
    assert_eq!(report.entries[0].metrics[1].value, Some(MeasurementValue::Integer("0".into())));
    assert!(report.limitations.contains(&t!("view.connection_outcomes.limit.lifetime")));
}

#[test]
fn missing_archived_rows_do_not_rewrite_counts_or_create_healthy_absence() {
    let mut raw = snapshot(180000);
    raw.observations.clear();
    assert!(explain(&raw, true).is_err());
    let report = explain(&raw, false).unwrap();
    assert_eq!((report.native_rows, report.observed_rows, report.mapped_rows), (1, 0, 0));
    assert_eq!(report.sources[0].state, SourceState::Partial);
    assert!(report.entries.is_empty());
    assert!(report.limitations.contains(&t!("view.connection_outcomes.limit.read")));
    assert_eq!(raw.sources[0].records, 1);
}

#[test]
fn unavailable_and_unrelated_native_counters_keep_their_original_coverage() {
    let mut raw = snapshot(180000);
    raw.observations.clear();
    raw.sources[0] =
        StateSourceCoverage::unavailable("pg_stat_database".into(), culprit_core::ErrorCode::PermissionDenied, None);
    let report = explain(&raw, true).unwrap();
    assert_eq!(report.sources[0].state, SourceState::Unavailable);
    assert!(report.entries.is_empty());
    let mut raw = snapshot(180000);
    raw.observations[0].counters.identity = NativeIdentity::new("other-counter");
    let report = explain(&raw, true).unwrap();
    assert_eq!((report.native_rows, report.observed_rows, report.mapped_rows), (1, 1, 0));
    assert_eq!(report.sources[0].state, SourceState::Collected);
    assert!(report.limitations.contains(&t!("view.connection_outcomes.limit.no_facts")));
}
