use super::*;
use crate::{
    read::{OwnedRead, test_support::Read},
    rows::Rec,
};
use culprit_core::{
    ConnectionOutcomeMetricKind, DatabaseIdentity, Engine, ErrorCode, MeasurementScope, ResetMarker, SourceState,
};
use mysql_async::Value;

fn target() -> CollectionTarget {
    DatabaseIdentity::server(Engine::Mysql, "app", "fixture.invalid", 3306, None).into()
}

fn row(name: &str, value: Value) -> Rec {
    Rec::from_values(vec![("variable_name", Value::from(name)), ("value", value)])
}

fn snapshot() -> Box<Snapshot> {
    let mut snapshot: Box<Snapshot> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/mysql-demo.json")))
            .unwrap();
    snapshot.server.database = "app".into();
    snapshot.bind_identity(target().database().unwrap().clone());
    snapshot.workload = None;
    snapshot.operations = Default::default();
    snapshot.pressure = Some(Default::default());
    snapshot.sources.clear();
    snapshot
}

#[tokio::test]
async fn native_points_remain_provisional_and_keep_exact_server_scope_without_probe_ages_or_grades() {
    let mut reader = Read::with(vec![Ok(vec![
        row("Connections", Value::from("18446744073709551615")),
        row("Aborted_clients", Value::UInt(0)),
        row("Connection_errors_select", Value::UInt(9_007_199_254_740_993)),
    ])]);
    let reading = read(&mut reader, target()).await.unwrap();
    assert_eq!(reader.query_count(), 1);
    assert_eq!(reader.closed, 0);
    assert!(reading.snapshot.is_valid());
    assert!(reading.snapshot.context.native().is_none(), "client acquisition bounds supply no native member or clock");
    for point in &reading.snapshot.observations {
        assert_eq!(point.counters.scope, MeasurementScope::Server);
        assert!(point.counters.counters.iter().all(|counter| counter.reset == ResetMarker::Unknown));
    }
    for kind in [
        ConnectionOutcomeMetricKind::ConnectionAttempts,
        ConnectionOutcomeMetricKind::AbortedClientConnections,
        ConnectionOutcomeMetricKind::ConnectionErrorsSelect,
    ] {
        assert!(reading.snapshot.rate(kind).is_none());
    }
    let expected = reading.snapshot.clone();
    let mut snapshot = snapshot();
    let shutdown = reader.close().await;
    settle(&mut snapshot, Ok(reading), shutdown);
    assert_eq!(reader.closed, 1);
    let actual = snapshot.pressure.as_ref().unwrap().connection_outcomes.as_ref().unwrap();
    assert_eq!(actual, &expected);
    assert_eq!(actual.observations[0].counters.counters[0].value.integer(), Some(u64::MAX));
    assert_eq!(actual.observations[1].counters.counters[0].value.integer(), Some(0));
    assert_eq!(actual.observations[2].counters.counters[0].value.integer(), Some(9_007_199_254_740_993));
    assert_eq!(actual.sources[0].state, SourceState::Partial);
    assert!(snapshot.pressure.as_ref().unwrap().signals.is_empty(), "native attempts are not inferred rates or grades");
}

#[tokio::test]
async fn native_denial_unsupported_timeout_and_cancel_preserve_original_source_failure_without_fallback() {
    for error in [
        Error::new(ErrorCode::PermissionDenied, "native denied"),
        Error::new(ErrorCode::Unsupported, "native unsupported"),
        CollectionStop::TimedOut.error(),
        CollectionStop::Cancelled.error(),
        CollectionStop::QueryLimit.error(),
    ] {
        let expected_code = error.code;
        let expected_stop = match error.detail {
            Some(culprit_core::ErrorDetail::CollectionStopped(stop)) => Some(stop),
            _ => None,
        };
        let mut reader = Read::with(vec![Err(error)]);
        let reading = read(&mut reader, target()).await.unwrap();
        assert_eq!(reader.query_count(), 1);
        assert!(reading.snapshot.observations.is_empty());
        assert_eq!(reading.snapshot.sources[0].state, SourceState::Unavailable);
        assert_eq!(reading.snapshot.sources[0].error_code, Some(expected_code));
        assert_eq!(reading.snapshot.sources[0].stopped, expected_stop);
        let mut snapshot = snapshot();
        let shutdown = reader.close().await;
        settle(&mut snapshot, Ok(reading), shutdown);
        assert!(snapshot.pressure.as_ref().unwrap().connection_outcomes.as_ref().unwrap().observations.is_empty());
        assert_eq!(reader.closed, 1);
        assert!(reader.replies.is_empty());
    }
}

#[tokio::test]
async fn unconfirmed_shared_reader_discards_provisional_native_points_and_cannot_claim_an_empty_success() {
    let mut reader = Read::with(vec![Ok(vec![row("Aborted_connects", Value::UInt(9))])]);
    reader.shutdown = Some(SourceShutdown::Unconfirmed);
    let reading = read(&mut reader, target()).await.unwrap();
    let mut snapshot = snapshot();
    let shutdown = reader.close().await;
    settle(&mut snapshot, Ok(reading), shutdown);
    let native = snapshot.pressure.as_ref().unwrap().connection_outcomes.as_ref().unwrap();
    assert!(native.is_valid());
    assert!(native.observations.is_empty());
    assert_eq!(native.sources[0].state, SourceState::Unavailable);
    assert_eq!(native.sources[0].stopped, Some(CollectionStop::CancellationUnconfirmed));
    assert!(
        snapshot
            .sources
            .iter()
            .any(|source| source.source == sources::CONNECTIONS.name && source.state == SourceState::Unavailable)
    );
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn absent_variables_remain_a_partial_unknown_roster_and_terminal_zero_budget_starts_no_native_work() {
    let mut reader = Read::with(vec![Ok(vec![])]);
    let reading = read(&mut reader, target()).await.unwrap();
    assert!(reading.snapshot.observations.is_empty());
    assert_eq!(reading.snapshot.sources[0].state, SourceState::Partial);
    assert_eq!(reading.snapshot.sources[0].records, 0);
    assert!(!reading.snapshot.sources[0].complete_scope);
    let mut reader = Read::with(vec![]).with_query_budget(0, 0);
    let reading = read(&mut reader, target()).await.unwrap();
    assert!(reader.queries.is_empty());
    assert_eq!(reading.snapshot.sources[0].stopped, Some(CollectionStop::QueryLimit));
    assert_eq!(reading.snapshot.sources[0].state, SourceState::Unavailable);
}

#[tokio::test]
async fn a_failed_replacement_cannot_retain_earlier_positive_outcomes() {
    let mut snapshot = snapshot();
    let mut reader = Read::with(vec![Ok(vec![row("Connections", Value::UInt(5))])]);
    let reading = read(&mut reader, target()).await;
    settle(&mut snapshot, reading, reader.close().await);
    assert!(!snapshot.pressure.as_ref().unwrap().connection_outcomes.as_ref().unwrap().observations.is_empty());

    let mut reader = Read::with(vec![Err(Error::new(ErrorCode::PermissionDenied, "native denied"))]);
    let reading = read(&mut reader, target()).await;
    settle(&mut snapshot, reading, reader.close().await);
    let current = snapshot.pressure.as_ref().unwrap().connection_outcomes.as_ref().unwrap();
    assert!(current.is_valid());
    assert!(current.observations.is_empty());
    assert_eq!(current.sources[0].state, SourceState::Unavailable);
    assert_eq!(current.sources[0].records, 0);
    assert_eq!(current.sources[0].error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(snapshot.source_outcome(sources::CONNECTIONS.name).unwrap().state, SourceState::Unavailable);
    let recorded_notes = snapshot.notes.len();
    assert!(recorded_notes > 0);

    settle(&mut snapshot, Err(CollectionStop::InvalidPage.error()), SourceShutdown::Quiescent);
    assert!(snapshot.pressure.as_ref().unwrap().connection_outcomes.is_none());
    assert_eq!(snapshot.source_outcome(sources::CONNECTIONS.name).unwrap().state, SourceState::Unavailable);
    assert!(snapshot.notes.len() > recorded_notes, "previous failure notes remain separate from current coverage");

    let recorded_notes = serde_json::to_value(&snapshot.notes).unwrap();
    let mut reader = Read::with(vec![Ok(vec![row("Connections", Value::UInt(0))])]);
    let reading = read(&mut reader, target()).await;
    settle(&mut snapshot, reading, reader.close().await);
    let current = snapshot.pressure.as_ref().unwrap().connection_outcomes.as_ref().unwrap();
    assert!(current.is_valid());
    assert_eq!(current.sources[0].records, 1);
    assert_eq!(current.sources[0].error_code, None);
    assert_eq!(current.observations[0].counters.counters[0].value.integer(), Some(0));
    let outcome = snapshot.source_outcome(sources::CONNECTIONS.name).unwrap();
    assert_eq!(outcome.state, SourceState::Collected);
    assert!(outcome.reason.is_none());
    assert_eq!(serde_json::to_value(&snapshot.notes).unwrap(), recorded_notes);
}
