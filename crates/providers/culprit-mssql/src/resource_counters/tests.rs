use super::*;
use crate::{
    memory_clerks::tests::row as memory_row,
    resources::test_support::FakeRead,
    schedulers::{fixture_columns, tests::row as scheduler_row},
};
use culprit_core::{Error, MeasurementScope, SourceState};

fn successful() -> FakeRead {
    FakeRead::new(vec![Ok(vec![memory_row()]), Ok(fixture_columns()), Ok(vec![scheduler_row()])])
}
fn state(coverage: &Coverage, name: &str) -> SourceState {
    coverage.outcomes().into_iter().find(|source| source.source == name).unwrap().state
}

#[tokio::test]
async fn the_default_policy_adds_no_queries_and_records_sources_as_not_requested() {
    let mut reader = FakeRead::new(vec![]);
    let (counters, coverage) = collect_from(&mut reader, false, true, None).await;
    assert!(counters.is_none());
    assert!(reader.calls.is_empty());
    assert_eq!(reader.closed, 0);
    assert_eq!(state(&coverage, sources::MEMORY_CLERKS.name), SourceState::NotApplicable);
    assert_eq!(state(&coverage, sources::SCHEDULERS.name), SourceState::NotApplicable);
}

#[tokio::test]
async fn explicitly_requested_facts_are_staged_through_confirmed_close_without_creating_pressure_verdicts() {
    let mut reader = successful();
    let (counters, coverage) = collect_from(&mut reader, true, true, None).await;
    let counters = counters.unwrap();
    assert_eq!(counters.len(), 2);
    assert!(counters.iter().all(|sample| sample.scope == MeasurementScope::Server && sample.is_valid()));
    assert!(counters.iter().all(|sample| !sample.properties.contains_key("query_id")));
    assert_eq!(state(&coverage, sources::MEMORY_CLERKS.name), SourceState::Collected);
    assert_eq!(state(&coverage, sources::SCHEDULERS.name), SourceState::Collected);
    assert_eq!(reader.closed, 1);
    assert_eq!(reader.calls.len(), MAX_QUERIES as usize);
    assert_eq!(reader.calls[0].limit, MAX_ROWS_PER_SOURCE + 1);
    assert_eq!(reader.calls[2].limit, MAX_ROWS_PER_SOURCE + 1);
    let budget = budget();
    budget.validate().unwrap();
    assert_eq!(budget.max_queries, 3);
    assert_eq!(budget.max_snapshot_rows, MAX_ROWS_PER_SOURCE as u32);
    assert_eq!(budget.statement_timeout_ms, STATEMENT_TIMEOUT_MS);
    assert_eq!(budget.total_timeout_ms, TOTAL_TIMEOUT_MS);
}

#[tokio::test]
async fn an_unconfirmed_shutdown_discards_every_staged_source_instead_of_exporting_apparent_success() {
    let mut reader = successful();
    reader.shutdown = SourceShutdown::Unconfirmed;
    let (counters, coverage) = collect_from(&mut reader, true, true, None).await;
    assert!(counters.is_none());
    assert_eq!(state(&coverage, sources::MEMORY_CLERKS.name), SourceState::Unavailable);
    assert_eq!(state(&coverage, sources::SCHEDULERS.name), SourceState::Unavailable);
    assert_eq!(reader.closed, 1);
    assert_eq!(reader.calls.len(), 3);
}

#[tokio::test]
async fn one_completed_permission_failure_does_not_erase_the_other_native_source() {
    let denied = || Error::new(ErrorCode::PermissionDenied, "fixture denied").into();
    for (replies, failed, available) in [
        (
            vec![Err(denied()), Ok(fixture_columns()), Ok(vec![scheduler_row()])],
            sources::MEMORY_CLERKS.name,
            sources::SCHEDULERS.name,
        ),
        (vec![Ok(vec![memory_row()]), Err(denied())], sources::SCHEDULERS.name, sources::MEMORY_CLERKS.name),
    ] {
        let mut reader = FakeRead::new(replies);
        let (counters, coverage) = collect_from(&mut reader, true, true, None).await;
        assert_eq!(counters.unwrap().len(), 1);
        assert_eq!(state(&coverage, failed), SourceState::Unavailable);
        assert_eq!(state(&coverage, available), SourceState::Collected);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn a_deadline_or_uncertain_read_stops_further_queries_and_does_not_return_empty_measured_values() {
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
        CollectionStop::PageLimit,
        CollectionStop::Pressure,
        CollectionStop::PressureUnavailable,
        CollectionStop::SourceChanged,
    ] {
        let mut reader = FakeRead::new(vec![Err(crate::read::ReadError::Stopped(stop))]);
        let (counters, coverage) = collect_from(&mut reader, true, true, None).await;
        assert!(counters.is_none());
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.closed, 1);
        assert_eq!(state(&coverage, sources::MEMORY_CLERKS.name), SourceState::Unavailable);
        assert_eq!(state(&coverage, sources::SCHEDULERS.name), SourceState::Unavailable);
    }
}

#[tokio::test]
async fn authentication_or_transport_failures_do_not_start_an_independent_replacement_read() {
    for code in [ErrorCode::Timeout, ErrorCode::AuthFailed, ErrorCode::ConnectionFailed] {
        let mut reader = FakeRead::new(vec![Err(Error::new(code, "fixture native failure").into())]);
        let (counters, coverage) = collect_from(&mut reader, true, true, None).await;
        assert!(counters.is_none());
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.closed, 1);
        assert_eq!(state(&coverage, sources::MEMORY_CLERKS.name), SourceState::Unavailable);
        assert_eq!(state(&coverage, sources::SCHEDULERS.name), SourceState::Unavailable);
    }
}

#[tokio::test]
async fn unsupported_memory_versions_do_not_probe_missing_columns_or_disable_supported_schedulers() {
    let mut reader = FakeRead::new(vec![Ok(fixture_columns()), Ok(vec![scheduler_row()])]);
    let (counters, coverage) = collect_from(&mut reader, true, false, None).await;
    assert_eq!(counters.unwrap().len(), 1);
    assert_eq!(state(&coverage, sources::MEMORY_CLERKS.name), SourceState::NotApplicable);
    assert_eq!(state(&coverage, sources::SCHEDULERS.name), SourceState::Collected);
    assert_eq!(reader.calls.len(), 2);
    assert!(reader.calls.iter().all(|call| !call.sql.contains("sys.dm_os_memory_clerks")));
}

#[tokio::test]
async fn a_native_source_overflow_is_not_truncated_into_healthy_or_zero_allocation() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![memory_row(); MAX_ROWS_PER_SOURCE + 1]),
        Ok(fixture_columns()),
        Ok(vec![scheduler_row()]),
    ]);
    let (counters, coverage) = collect_from(&mut reader, true, true, None).await;
    assert_eq!(counters.unwrap().len(), 1);
    assert_eq!(state(&coverage, sources::MEMORY_CLERKS.name), SourceState::Unavailable);
    assert_eq!(state(&coverage, sources::SCHEDULERS.name), SourceState::Collected);
    assert_eq!(reader.calls[0].limit, MAX_ROWS_PER_SOURCE + 1);
}
