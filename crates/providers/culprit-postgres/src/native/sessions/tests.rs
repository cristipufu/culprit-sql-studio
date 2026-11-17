use super::*;
use culprit_core::{ConnectionOutcomeMetricKind, MeasurementScope, ResetMarker};

pub(crate) fn values() -> Value {
    serde_json::json!({
        "datid":42,"datname":"fixture","stats_reset":"2020-01-01T00:00:00Z",
        "counter_observed_at":"2020-01-02T00:00:00Z","native_postmaster_started_at":"2020-01-01T23:00:00Z",
        "native_server_version_num":"180000","native_block_bytes":"8192",
        "sessions":9007199254740993u64,"sessions_abandoned":0,"sessions_fatal":null,"sessions_killed":7,
        "session_time":123456.125,"active_time":0.0,"idle_in_transaction_time":12.5
        ,"blks_hit":100,"blks_read":0,"temp_bytes":512,"xact_commit":10,"xact_rollback":0,"deadlocks":0
    })
}
pub(crate) fn row(values: Value) -> Row {
    Row::from_fields([
        ("native_source".into(), Some(SOURCE.into())),
        ("native_values".into(), Some(values.to_string())),
    ])
}
pub(crate) fn snapshot(values: Value) -> ConnectionOutcomeSnapshot {
    ConnectionOutcomeSnapshot {
        target: DatabaseIdentity::server(culprit_core::Engine::Postgres, "fixture", "fixture.invalid", 5432, None)
            .into(),
        context: ObservationReadContext::Collector {
            observed_from: "2026-01-01T00:00:00Z".into(),
            observed_until: "2026-01-01T00:00:01Z".into(),
        },
        sources: vec![StateSourceCoverage::observed(SOURCE.into(), 1, true)],
        observations: vec![decode(&row(values), "fixture").unwrap()],
    }
}

#[test]
fn scan_and_collection_decode_the_same_native_row_without_losing_exact_counts_or_nulls() {
    let native = row(values());
    let scan = decode(&native, "fixture").unwrap();
    let collected = super::super::operational::decode_values(
        SOURCE.into(),
        values(),
        super::super::operational::CounterReadContext {
            database_oid: "42",
            started_at: "2020-01-01T23:00:00Z",
            block_bytes: 8192,
            version: 180000,
        },
    )
    .unwrap();
    let TelemetryRecord::Operational(collected) = collected else { panic!() };
    assert_eq!(scan, ConnectionOutcomeObservation::from_operational(&collected).unwrap());
    assert_eq!(scan.counters.scope, MeasurementScope::Database);
    let by_key = |key: &str| scan.counters.counters.iter().find(|counter| counter.key == key);
    assert_eq!(by_key("sessions").unwrap().value.integer(), Some(9007199254740993));
    assert_eq!(by_key("sessions_abandoned").unwrap().value.integer(), Some(0));
    assert!(by_key("sessions_fatal").is_none());
    assert_eq!(
        by_key("sessions_killed").unwrap().reset,
        ResetMarker::At(canonical_time("2020-01-01T00:00:00Z").unwrap())
    );
    assert!(!scan.counters.identity.qualifiers.contains_key("counter_observed_at"));
}

#[test]
fn the_counter_reset_not_the_new_process_or_collector_clock_controls_a_lifetime_rate() {
    let mut read = snapshot(values());
    let rate = read.rate(ConnectionOutcomeMetricKind::SessionsEstablished).unwrap();
    assert_eq!(rate.seconds, 86400.0);
    assert_eq!(rate.per_second, 9007199254740993u64 as f64 / 86400.0);
    for counter in &mut read.observations[0].counters.counters {
        counter.reset = ResetMarker::Unknown;
    }
    assert!(read.rate(ConnectionOutcomeMetricKind::SessionsEstablished).is_none());
    assert_eq!(
        read.observations[0].counters.counters[0].value,
        snapshot(values()).observations[0].counters.counters[0].value
    );
}

#[test]
fn pre14_absent_fields_are_unsupported_and_foreign_or_malformed_native_rows_are_rejected() {
    let mut legacy = values();
    legacy["native_server_version_num"] = "130000".into();
    for key in [
        "sessions",
        "sessions_abandoned",
        "sessions_fatal",
        "sessions_killed",
        "session_time",
        "active_time",
        "idle_in_transaction_time",
    ] {
        legacy.as_object_mut().unwrap().remove(key);
    }
    legacy["xact_commit"] = 1.into();
    let legacy = snapshot(legacy);
    assert!(
        legacy
            .profile(&legacy.observations[0])
            .unwrap()
            .metrics
            .iter()
            .all(|metric| metric.availability == culprit_core::ConnectionOutcomeAvailability::Unsupported)
    );
    for (key, value) in [
        ("datname", Value::from("another")),
        ("datid", Value::from(0)),
        ("native_block_bytes", Value::from("0")),
        ("counter_observed_at", Value::from("invalid")),
    ] {
        let mut invalid = values();
        invalid[key] = value;
        assert!(decode(&row(invalid), "fixture").is_err(), "{key}");
    }
}

struct Reader {
    calls: usize,
    reply: Option<Result<Vec<Row>>>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        self.calls += 1;
        assert_eq!(limit, 2);
        assert!(sql.contains("pg_catalog.pg_stat_database") && sql.contains("pg_catalog.current_database()"));
        assert!(sql.contains("clock_timestamp()") && !sql.contains("pg_stat_activity"));
        self.reply.take().expect("the source must not retry or read another native family")
    }
}

#[tokio::test]
async fn acquisition_uses_one_held_reader_statement_and_terminal_failures_do_not_retry() {
    let target = DatabaseIdentity::server(culprit_core::Engine::Postgres, "fixture", "fixture.invalid", 5432, None);
    let mut reader = Reader { calls: 0, reply: Some(Ok(vec![row(values())])) };
    let read = acquire(&mut reader, target.clone()).await.unwrap();
    assert!(read.is_valid());
    assert_eq!(reader.calls, 1);
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::SourceChanged] {
        let mut reader = Reader { calls: 0, reply: Some(Err(stop.error())) };
        let error = acquire(&mut reader, target.clone()).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.calls, 1);
    }
}
