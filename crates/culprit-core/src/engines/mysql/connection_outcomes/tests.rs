use super::*;
use crate::{
    ConnectionOutcomeObservation, ConnectionOutcomeSnapshot, DatabaseIdentity, Engine, ResetMarker, telemetry::*,
};
use std::time::{Duration, SystemTime};

fn row(name: &str, value: u64, nature: CounterNature) -> NativeCounterSet {
    let mut counter =
        NativeCounterSample::gauge(name, value, CounterUnit::Count, MeasurementScope::Server, "global-status", name);
    counter.nature = nature;
    NativeCounterSet {
        identity: NativeIdentity::new(name),
        scope: MeasurementScope::Server,
        counters: vec![counter],
        properties: Default::default(),
    }
}

#[test]
fn attempted_failed_and_aborted_established_connections_are_distinct_server_populations() {
    for (name, expected) in [
        ("Connections", ConnectionOutcomeMetricKind::ConnectionAttempts),
        ("Aborted_connects", ConnectionOutcomeMetricKind::FailedConnectionAttempts),
        ("Aborted_clients", ConnectionOutcomeMetricKind::AbortedClientConnections),
        ("Connection_errors_select", ConnectionOutcomeMetricKind::ConnectionErrorsSelect),
    ] {
        let native = row(name, 1, CounterNature::Cumulative);
        let policy = Engine::Mysql.spec().connection_outcome_profile("global-status", &native).unwrap();
        assert_eq!(policy.metrics.len(), 1);
        assert_eq!(policy.metrics[0].native_key, name);
        assert_eq!(policy.metrics[0].kind, expected);
        assert_eq!(policy.scope, MeasurementScope::Server);
        assert_eq!(policy.metrics[0].availability, ConnectionOutcomeAvailability::Supported);
        assert!(policy.observed_at.is_none());
        assert_ne!(policy.metrics[0].kind, ConnectionOutcomeMetricKind::SessionsEstablished);
    }
}

#[test]
fn source_schema_and_engine_ownership_do_not_fall_back_from_lookalike_names() {
    let native = row("Connections", 0, CounterNature::Cumulative);
    assert!(Engine::Mysql.spec().connection_outcome_profile("session-status", &native).is_none());
    assert!(
        Engine::Mysql
            .spec()
            .connection_outcome_profile("global-status", &row("Questions", 0, CounterNature::Cumulative))
            .is_none()
    );
    for engine in Engine::ALL {
        assert_eq!(
            engine.spec().connection_outcome_profile("global-status", &native).is_some(),
            engine == Engine::Mysql,
            "{engine:?}"
        );
    }
}

#[test]
fn unknown_reset_and_a_collector_clock_never_become_an_uptime_based_rate_or_database_total() {
    let mut native = row("Aborted_connects", 9_007_199_254_740_993, CounterNature::Cumulative);
    native.properties.insert("Uptime".into(), "864000".into());
    native.properties.insert("observed_at".into(), "2020-01-01T12:00:00Z".into());
    let mut snapshot = ConnectionOutcomeSnapshot {
        target: DatabaseIdentity::server(Engine::Mysql, "app", "fixture.invalid", 3306, None).into(),
        context: ObservationReadContext::collector(
            SystemTime::UNIX_EPOCH,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        )
        .unwrap(),
        sources: vec![StateSourceCoverage::observed("global-status".into(), 1, false)],
        observations: vec![ConnectionOutcomeObservation { source: "global-status".into(), counters: native }],
    };
    assert!(snapshot.is_valid());
    assert!(snapshot.rate(ConnectionOutcomeMetricKind::FailedConnectionAttempts).is_none());
    assert_eq!(snapshot.observations[0].counters.counters[0].value.integer(), Some(9_007_199_254_740_993));
    snapshot.observations[0].counters.counters[0].reset = ResetMarker::At("2020-01-01T00:00:00Z".into());
    assert!(
        snapshot.rate(ConnectionOutcomeMetricKind::FailedConnectionAttempts).is_none(),
        "the SHOW source has no native counter clock"
    );
    snapshot.observations[0].counters.scope = MeasurementScope::Database;
    snapshot.observations[0].counters.counters[0].scope = MeasurementScope::Database;
    assert!(!snapshot.is_valid(), "global-status cannot charge the configured database");
}

#[test]
fn current_thread_gauges_keep_their_native_nature_and_unknown_statuses_gain_no_meaning() {
    for (name, expected) in [
        ("Threads_connected", ConnectionOutcomeMetricKind::ThreadsConnected),
        ("Threads_running", ConnectionOutcomeMetricKind::ThreadsRunning),
    ] {
        let native = row(name, 0, CounterNature::Gauge);
        let profile = Engine::Mysql.spec().connection_outcome_profile("global-status", &native).unwrap();
        assert_eq!(profile.metrics[0].kind, expected);
        assert_eq!(profile.metrics[0].kind.nature(), CounterNature::Gauge);
    }
    let native = row("sessions_fatal", 2, CounterNature::Cumulative);
    assert!(Engine::Mysql.spec().connection_outcome_profile("global-status", &native).is_none());
}
