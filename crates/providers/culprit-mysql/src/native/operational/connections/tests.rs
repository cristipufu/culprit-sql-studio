use super::*;
use crate::{
    read::{OwnedRead, test_support::Read},
    rows::Rec,
};
use culprit_core::{Engine, MeasurementScope, ResetMarker, telemetry::*};
use mysql_async::Value;

fn row(name: &str, value: Value) -> Rec {
    Rec::from_values(vec![("variable_name", Value::from(name)), ("value", value)])
}

#[tokio::test]
async fn scan_and_retained_collection_share_one_exact_native_decoder_without_performance_schema() {
    let rows = vec![
        row("Connections", Value::from("18446744073709551615")),
        row("Aborted_connects", Value::UInt(0)),
        row("Connection_errors_select", Value::UInt(9_007_199_254_740_993)),
        row("Threads_connected", Value::UInt(1)),
    ];
    let expected = rows.iter().map(|row| Kind::Status.decode(row, "irrelevant-database").unwrap()).collect::<Vec<_>>();
    let mut reader = Read::with(vec![Ok(rows)]).with_query_budget(1, 0);
    let actual = read(&mut reader).await.unwrap();
    assert_eq!(actual.into_iter().map(TelemetryRecord::Operational).collect::<Vec<_>>(), expected);
    assert_eq!(reader.query_count(), 1);
    assert_eq!(reader.closed, 0, "the outer scan owns this shared reader");
    let (sql, parameters, row_limit) = &reader.queries[0];
    assert!(sql.starts_with("SHOW GLOBAL STATUS"));
    assert!(parameters.is_empty());
    assert_eq!(*row_limit, 13);
    assert!(!sql.contains("performance_schema") && !sql.contains("Uptime"));
    for name in [
        "Connections",
        "Aborted_connects",
        "Aborted_clients",
        "Connection_errors_accept",
        "Connection_errors_internal",
        "Connection_errors_max_connections",
        "Connection_errors_peer_address",
        "Connection_errors_select",
        "Connection_errors_tcpwrap",
    ] {
        assert!(sql.contains(name), "{name}");
    }
}

#[tokio::test]
async fn measured_zero_and_missing_optional_variables_remain_distinct_server_counters_with_unknown_reset() {
    let mut reader = Read::with(vec![Ok(vec![row("Aborted_clients", Value::UInt(0))])]);
    let points = read(&mut reader).await.unwrap();
    assert_eq!(points.len(), 1);
    let OperationalFact::CounterSet(set) = &points[0].fact else { panic!() };
    assert_eq!(set.scope, MeasurementScope::Server);
    assert_eq!(set.counters[0].value.integer(), Some(0));
    assert_eq!(set.counters[0].reset, ResetMarker::Unknown);
    let profile = Engine::Mysql.spec().connection_outcome_profile(&points[0].source, set).unwrap();
    assert_eq!(profile.metrics.len(), 1);
    assert_eq!(profile.metrics[0].native_key, "Aborted_clients");
    assert!(profile.observed_at.is_none());
}

#[tokio::test]
async fn duplicate_unrequested_or_malformed_native_values_stop_without_fallback_or_zero_substitution() {
    for rows in [
        vec![row("Aborted_connects", Value::UInt(1)), row("ABORTED_CONNECTS", Value::UInt(2))],
        vec![row("Uptime", Value::UInt(86400))],
        vec![row("Aborted_connects", Value::Double(1.0))],
        vec![row("Aborted_clients", Value::from("18446744073709551616"))],
        vec![row("Connections", Value::Int(-1))],
    ] {
        let mut reader = Read::with(vec![Ok(rows)]);
        assert_eq!(
            read(&mut reader).await.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
        );
        assert_eq!(reader.query_count(), 1);
        assert!(reader.replies.is_empty());
    }
}
