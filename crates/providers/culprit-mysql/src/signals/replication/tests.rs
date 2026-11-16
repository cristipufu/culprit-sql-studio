use super::*;
use crate::{
    read::{OwnedRead, test_support::Read},
    rows::Rec,
};
use culprit_core::{ErrorCode, Pressure, SourceState};
use mysql_async::Value;

fn row(channel: &str) -> Rec {
    Rec::from_values(vec![
        ("Channel_Name", channel.into()),
        ("Source_Host", "source.example.invalid".into()),
        ("Replica_IO_Running", "Yes".into()),
        ("Replica_SQL_Running", "Yes".into()),
        ("Seconds_Behind_Source", Value::UInt(3_630)),
        ("SQL_Delay", Value::UInt(3_600)),
        ("Last_IO_Errno", Value::UInt(1045)),
        ("Last_SQL_Errno", Value::UInt(0)),
        ("Read_Source_Log_Pos", Value::UInt(9_007_199_254_740_993)),
    ])
}

fn snapshot() -> Box<Snapshot> {
    let mut snapshot: Box<Snapshot> =
        serde_json::from_str(include_str!("../../../../../../fixtures/mysql-demo.json")).unwrap();
    snapshot.pressure = Some(Pressure::default());
    snapshot.sources.retain(|source| !culprit_core::same_source(&source.source, sources::REPLICATION.name));
    snapshot
}

#[tokio::test]
async fn one_show_response_supplies_raw_channels_and_the_same_legacy_projection_without_owning_close() {
    let mut reader = Read::with(vec![Ok(vec![row("orders"), row("group_replication_recovery")])]);
    let reading = read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap();
    assert!(reading.error.is_none());
    assert!(reading.snapshot.is_valid());
    assert!(matches!(reading.snapshot.context, ObservationReadContext::Collector { .. }));
    assert!(reading.snapshot.context.native().is_none());
    assert_eq!(reading.snapshot.sources[0].records, 2);
    assert_eq!(reader.queries.len(), 1);
    assert_eq!(reader.closed, 0);
    let mut snapshot = snapshot();
    assert!(snapshot.pressure.as_ref().unwrap().replication.is_none());
    assert!(snapshot.source_outcome(sources::REPLICATION.name).is_none());
    let shutdown = reader.close().await;
    settle(&mut snapshot, Ok(reading), shutdown);
    let pressure = snapshot.pressure.as_ref().unwrap();
    assert_eq!(pressure.replicas.len(), 1, "group channels stay raw facts, not an asynchronous legacy verdict");
    assert_eq!(pressure.replicas[0].lag_seconds, Some(30.0));
    assert_eq!(pressure.replicas[0].delay_seconds, Some(3_600.0));
    assert_eq!(pressure.replicas[0].condition, culprit_core::ReplicaCondition::Replicating);
    let raw = pressure.replication.as_ref().unwrap();
    assert_eq!(raw.observations.len(), 2);
    let ReplicationFact::Native(native) = &raw.observations[0].fact else { panic!("native channel") };
    let received =
        native.measurements.counters.iter().find(|counter| counter.key == "received-source-log-position").unwrap();
    assert_eq!(received.value, MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(
        native.measurements.counters.iter().find(|counter| counter.key == "lag-seconds").unwrap().value.integer(),
        Some(3_630)
    );
    assert_eq!(
        native.measurements.counters.iter().find(|counter| counter.key == "io-error-code").unwrap().value.integer(),
        Some(1045)
    );
    assert!(raw.is_valid());
    assert_eq!(snapshot.source_outcome(sources::REPLICATION.name).unwrap().state, SourceState::Collected);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn native_empty_and_unavailable_are_distinct_completed_reads_without_retry_or_fallback() {
    let mut reader = Read::with(vec![Ok(vec![])]);
    let empty = read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap();
    assert!(empty.error.is_none());
    assert_eq!(empty.snapshot.sources[0].state, SourceState::Empty);
    assert!(empty.snapshot.sources[0].complete_scope);
    assert!(empty.snapshot.is_valid());
    let mut snapshot = self::snapshot();
    let shutdown = reader.close().await;
    settle(&mut snapshot, Ok(empty), shutdown);
    assert_eq!(snapshot.source_outcome(sources::REPLICATION.name).unwrap().state, SourceState::Empty);
    assert_eq!(reader.closed, 1);
    let errors = [
        Error::new(ErrorCode::PermissionDenied, "native permission denied"),
        CollectionStop::TimedOut.error(),
        CollectionStop::Cancelled.error(),
        CollectionStop::CancellationUnconfirmed.error(),
        CollectionStop::RowLimit.error(),
    ];
    for error in errors {
        let mut reader = Read::with(vec![Err(error.clone())]);
        let unavailable = read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap();
        assert_eq!(unavailable.error.as_ref().unwrap().detail, error.detail);
        assert_eq!(unavailable.snapshot.sources[0].state, SourceState::Unavailable);
        assert_eq!(unavailable.snapshot.sources[0].error_code, Some(error.code));
        assert!(!unavailable.snapshot.sources[0].complete_scope);
        assert!(unavailable.snapshot.observations.is_empty());
        assert!(unavailable.snapshot.is_valid());
        assert_eq!(reader.queries.len(), 1);
        assert_eq!(reader.closed, 0);
        let mut snapshot = self::snapshot();
        let shutdown = reader.close().await;
        settle(&mut snapshot, Ok(unavailable), shutdown);
        let outcome = snapshot.source_outcome(sources::REPLICATION.name).unwrap();
        assert_eq!(
            outcome.state,
            SourceState::Unavailable,
            "a decoded unavailable header is not a successful source part"
        );
        assert!(outcome.reason.as_ref().unwrap().contains(&error.message));
        assert!(snapshot.pressure.as_ref().unwrap().replicas.is_empty());
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn malformed_native_channel_never_becomes_healthy_empty_or_a_legacy_first_row() {
    let malformed = row("orders").with_fields(&[("read_source_log_pos", "18446744073709551616".into())]);
    let mut reader = Read::with(vec![Ok(vec![row("a"), malformed])]);
    let unavailable = read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap();
    assert_eq!(unavailable.snapshot.sources[0].state, SourceState::Unavailable);
    assert_eq!(unavailable.snapshot.sources[0].stopped, Some(CollectionStop::InvalidPage));
    assert!(unavailable.snapshot.observations.is_empty());
    let mut snapshot = snapshot();
    let shutdown = reader.close().await;
    settle(&mut snapshot, Ok(unavailable), shutdown);
    assert!(snapshot.pressure.as_ref().unwrap().replicas.is_empty());
    assert_eq!(snapshot.source_outcome(sources::REPLICATION.name).unwrap().state, SourceState::Unavailable);
    assert_eq!(reader.queries.len(), 1);
}

#[tokio::test]
async fn unconfirmed_outer_shutdown_withholds_both_positive_and_empty_replication_publication() {
    for rows in [vec![row("orders")], vec![]] {
        let mut reader = Read::with(vec![Ok(rows)]);
        reader.shutdown = Some(SourceShutdown::Unconfirmed);
        let reading = read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap();
        let mut snapshot = snapshot();
        let mut unrelated = Coverage::default();
        unrelated.completed(&sources::CONNECTIONS, 1);
        unrelated.source_failed(&sources::HISTOGRAMS, &Error::new(ErrorCode::PermissionDenied, "histograms denied"));
        unrelated.append_to(&mut snapshot);
        let previous = snapshot.sources.clone();
        assert!(snapshot.pressure.as_ref().unwrap().replication.is_none());
        assert!(snapshot.source_outcome(sources::REPLICATION.name).is_none());
        let shutdown = reader.close().await;
        settle(&mut snapshot, Ok(reading), shutdown);
        let pressure = snapshot.pressure.as_ref().unwrap();
        assert!(pressure.replicas.is_empty());
        let raw = pressure.replication.as_ref().unwrap();
        assert!(raw.observations.is_empty());
        assert_eq!(raw.sources[0].records, 0);
        assert_eq!(raw.sources[0].state, SourceState::Unavailable);
        assert_eq!(raw.sources[0].stopped, Some(CollectionStop::CancellationUnconfirmed));
        assert!(!raw.sources[0].complete_scope);
        assert!(raw.is_valid());
        assert_eq!(
            snapshot
                .sources
                .iter()
                .find(|source| culprit_core::same_source(&source.source, sources::REPLICATION.name))
                .unwrap()
                .state,
            SourceState::Unavailable
        );
        assert_eq!(reader.queries.len(), 1);
        assert_eq!(reader.closed, 1);
        for outcome in previous {
            assert_eq!(snapshot.source_outcome(&outcome.source), Some(&outcome));
        }
    }
}

#[tokio::test]
async fn a_native_failure_and_unconfirmed_shutdown_both_survive_without_a_successful_source_part() {
    for error in [
        Error::new(ErrorCode::PermissionDenied, "native replication permission denied"),
        CollectionStop::TimedOut.error(),
        CollectionStop::Cancelled.error(),
    ] {
        let mut reader = Read::with(vec![Err(error.clone())]);
        reader.shutdown = Some(SourceShutdown::Unconfirmed);
        let reading = read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap();
        assert_eq!(reading.error.as_ref().unwrap().detail, error.detail);
        let mut snapshot = snapshot();
        let shutdown = reader.close().await;
        settle(&mut snapshot, Ok(reading), shutdown);
        let outcome = snapshot.source_outcome(sources::REPLICATION.name).unwrap();
        assert_eq!(outcome.state, SourceState::Unavailable);
        let reason = outcome.reason.as_ref().unwrap();
        assert!(reason.contains(&error.message), "{reason}");
        assert!(reason.contains(&CollectionStop::CancellationUnconfirmed.error().message), "{reason}");
        let pressure = snapshot.pressure.as_ref().unwrap();
        assert!(pressure.replicas.is_empty());
        let raw = pressure.replication.as_ref().unwrap();
        assert!(raw.is_valid() && raw.observations.is_empty());
        assert_eq!(raw.sources[0].state, SourceState::Unavailable);
        assert_eq!(raw.sources[0].error_code, Some(error.code));
        assert_eq!(reader.queries.len(), 1);
        assert_eq!(reader.closed, 1);
        assert!(reader.replies.is_empty());
    }
}

#[tokio::test]
async fn raw_group_channels_are_collected_even_when_the_legacy_projection_has_no_rows() {
    let mut reader = Read::with(vec![Ok(vec![row("group_replication_recovery")])]);
    let reading = read(&mut reader, Flavor::Mysql, "8.4.11").await.unwrap();
    let mut snapshot = snapshot();
    let shutdown = reader.close().await;
    settle(&mut snapshot, Ok(reading), shutdown);
    assert!(snapshot.pressure.as_ref().unwrap().replicas.is_empty());
    assert_eq!(snapshot.pressure.as_ref().unwrap().replication.as_ref().unwrap().observations.len(), 1);
    assert_eq!(snapshot.source_outcome(sources::REPLICATION.name).unwrap().state, SourceState::Collected);
    assert_eq!(reader.queries.len(), 1);
    assert_eq!(reader.closed, 1);
}
