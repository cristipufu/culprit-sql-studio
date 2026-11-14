use super::test_support::{consumers, metadata, row, row_with};
use super::*;
use crate::read::test_support::Read;

#[tokio::test]
async fn baseline_keeps_ordinary_sessions_and_short_native_requests_without_owner_or_sql_walks() {
    let mut reader = Read::with(vec![Ok(consumers(true)), Ok(vec![row(1, "Query", true), row(2, "Sleep", false)])]);
    let frame = observe(&mut reader, &metadata(), &CollectionBudget::default()).await.unwrap();
    assert_eq!(frame.observations.len(), 2);
    assert!(frame.coverage.complete_scope);
    let active = &frame.observations[0];
    assert_eq!(active.elapsed_ms.as_deref(), Some("10"));
    assert!(active.statement_identity.is_some());
    assert!(active.open_transactions.is_none());
    assert_eq!(active.blocking_status, ActivityBlockingStatus::Unknown);
    assert!(matches!(active.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }));
    let idle = &frame.observations[1];
    assert!(idle.idle && idle.key.request.is_none() && idle.statement_identity.is_none());
    assert!(idle.open_transactions.is_none());
    assert_eq!(reader.queries.len(), 2);
    let sql = &reader.queries[1].0;
    assert!(sql.contains("t.PROCESSLIST_DB=?") && sql.find("t.PROCESSLIST_DB=?") < sql.find("LIMIT"));
    assert_eq!(reader.queries[1].1, vec![Value::from("shop")]);
    assert!(reader.queries.iter().all(|(sql, _, _)| !sql.to_lowercase().contains("sql_text")
        && !sql.contains("innodb_trx")
        && !sql.contains("data_lock_waits")));
}

#[tokio::test]
async fn missing_native_current_statement_stays_session_context_with_partial_coverage() {
    for enabled in [false, true] {
        let mut reader = Read::with(vec![Ok(consumers(enabled)), Ok(vec![row(1, "Query", false)])]);
        let frame = observe(&mut reader, &metadata(), &CollectionBudget::default()).await.unwrap();
        assert_eq!(frame.observations.len(), 1);
        let row = &frame.observations[0];
        assert!(!row.idle);
        assert!(row.key.request.is_none() && row.statement_identity.is_none() && row.elapsed_ms.is_none());
        assert!(row.request_counters.is_none() && row.open_transactions.is_none());
        assert!(row.waiting_on_lock.is_none());
        assert!(!frame.coverage.complete_scope);
        assert_eq!(frame.coverage.state, culprit_core::SourceState::Partial);
    }
}

#[tokio::test]
async fn a_forty_second_metadata_lock_point_survives_without_owner_or_sql_investigation() {
    let native = row_with(
        1,
        "Query",
        true,
        &[
            ("state", Value::from("Waiting for table metadata lock")),
            ("seconds", Value::from(40u64)),
            ("elapsed_ps", Value::from(40_000_000_000_000u64)),
        ],
    );
    let mut reader = Read::with(vec![Ok(consumers(true)), Ok(vec![native])]);
    let frame = observe(&mut reader, &metadata(), &CollectionBudget::default()).await.unwrap();
    let row = &frame.observations[0];
    assert_eq!(row.waiting_on_lock, Some(true));
    assert_eq!(row.elapsed_ms.as_deref(), Some("40000"));
    assert_eq!(row.wait.as_ref().unwrap().elapsed_ms.as_deref(), Some("40000"));
    assert_eq!(row.wait.as_ref().unwrap().native_type.as_deref(), Some("Waiting for table metadata lock"));
    assert!(row.wait.as_ref().unwrap().owners.is_empty());
    assert_eq!(row.blocking_status, ActivityBlockingStatus::Unknown);
    assert!(row.statement_identity.is_some());
    assert!(matches!(row.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }));
    assert_eq!(reader.queries.len(), 2);
    assert!(reader.queries.iter().all(|(sql, _, _)| !sql.to_lowercase().contains("sql_text")
        && !sql.contains("innodb_trx")
        && !sql.contains("data_lock_waits")));
}

#[tokio::test]
async fn a_metadata_lock_without_a_valid_current_event_stays_unknown_and_partial() {
    for (enabled, current) in [(false, true), (true, false)] {
        let native = row_with(
            1,
            "Query",
            current,
            &[("state", Value::from("Waiting for table metadata lock")), ("seconds", Value::from(40u64))],
        );
        let mut reader = Read::with(vec![Ok(consumers(enabled)), Ok(vec![native])]);
        let frame = observe(&mut reader, &metadata(), &CollectionBudget::default()).await.unwrap();
        let row = &frame.observations[0];
        assert!(row.waiting_on_lock.is_none());
        assert!(row.key.request.is_none() && row.statement_identity.is_none());
        assert!(row.wait.is_some(), "retain the unqualified native session status");
        assert_eq!(frame.coverage.state, culprit_core::SourceState::Partial);
        assert!(!frame.coverage.complete_scope);
        assert_eq!(reader.queries.len(), 2);
    }
}

#[test]
fn only_authoritative_native_status_or_already_read_owners_establish_a_lock_positive() {
    for state in LOCK_WAIT_STATES {
        assert_eq!(waiting_on_lock(Some(state), false), Some(true), "{state}");
    }
    assert_eq!(waiting_on_lock(Some("updating"), true), Some(true));
    for state in [None, Some("updating"), Some("Waiting for future clock"), Some("System lock"), Some("User lock")] {
        assert_eq!(waiting_on_lock(state, false), None, "{state:?}");
    }
}

#[tokio::test]
async fn a_known_table_level_lock_keeps_its_native_meaning_instead_of_becoming_metadata_lock() {
    let native = row_with(1, "Query", true, &[("state", Value::from("Waiting for table level lock"))]);
    let mut reader = Read::with(vec![Ok(consumers(true)), Ok(vec![native])]);
    let frame = observe(&mut reader, &metadata(), &CollectionBudget::default()).await.unwrap();
    let row = &frame.observations[0];
    assert_eq!(row.waiting_on_lock, Some(true));
    assert_eq!(row.wait.as_ref().unwrap().native_type.as_deref(), Some("Waiting for table level lock"));
    assert_eq!(reader.queries.len(), 2);
}

#[tokio::test]
async fn witness_and_native_visibility_restrictions_preserve_rows_without_complete_absence() {
    let budget = CollectionBudget { max_snapshot_rows: 1, ..Default::default() };
    let mut reader = Read::with(vec![Ok(consumers(true)), Ok(vec![row(1, "Query", true), row(2, "Query", true)])]);
    let frame = observe(&mut reader, &metadata(), &budget).await.unwrap();
    assert_eq!(frame.observations.len(), 1);
    assert_eq!(frame.native_rows, 2);
    assert_eq!(frame.coverage.stopped, Some(CollectionStop::RowLimit));
    assert_eq!(reader.queries[1].2, 2);
    let mut meta = metadata();
    meta.process_wide = false;
    let mut reader = Read::with(vec![Ok(consumers(true)), Ok(vec![row(1, "Query", true)])]);
    let frame = observe(&mut reader, &meta, &CollectionBudget::default()).await.unwrap();
    assert_eq!(frame.observations.len(), 1);
    assert_eq!(frame.coverage.error_code, Some(ErrorCode::PermissionDenied));
    assert!(!frame.coverage.complete_scope);
}

#[tokio::test]
async fn unavailable_or_cancelled_native_reads_do_not_run_optional_enrichment() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::ByteLimit,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let mut reader = Read::with(vec![Err(stop.error())]);
        let error = observe(&mut reader, &metadata(), &CollectionBudget::default()).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.queries.len(), 1);
    }
}
