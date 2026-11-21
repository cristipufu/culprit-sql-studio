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

#[test]
fn native_thread_default_schema_is_retained_independently_of_a_current_statement_schema_and_state_age() {
    for (command, current) in [("Query", true), ("Sleep", false)] {
        let row = row_with(
            1,
            command,
            current,
            &[
                ("db", Value::from("session-default")),
                ("request_db", Value::from("statement-default")),
                ("seconds", Value::from(999u64)),
            ],
        );
        let point = decode(&row, "session-default", true).unwrap().unwrap();
        let context = point.session_context.as_deref().unwrap();
        let Some(SessionNativeContext::Mysql(native)) = context.native.as_deref() else { panic!() };
        assert_eq!(native.default_database.as_deref(), Some("session-default"));
        assert_eq!(native.request_schema.as_deref(), current.then_some("statement-default"));
        assert_eq!(point.database.as_deref(), Some(if current { "statement-default" } else { "session-default" }));
        assert!(native.is_client_thread(point.command.as_deref()));
        assert!(context.login_age_ms.is_none() && context.client_library.is_none());
        assert!(point.session_counters.logical_reads.is_none());
    }
}

#[test]
fn internal_and_unknown_native_thread_classes_are_retained_without_becoming_clients_from_user_labels() {
    for (kind, name) in [
        ("BACKGROUND", "thread/sql/one_connection"),
        ("FOREGROUND", "thread/sql/slave_sql"),
        ("FOREGROUND", "thread/sql/thread_pool_worker"),
    ] {
        let row = row_with(
            1,
            "Query",
            true,
            &[
                ("thread_type", Value::from(kind)),
                ("thread_name", Value::from(name)),
                ("user_name", Value::from("ordinary-app-user")),
            ],
        );
        let point = decode(&row, "shop", true).unwrap().unwrap();
        let Some(SessionNativeContext::Mysql(native)) = point.session_context.as_deref().unwrap().native.as_deref()
        else {
            panic!()
        };
        assert!(!native.is_client_thread(point.command.as_deref()));
        assert_eq!(native.thread_type.as_deref(), Some(kind));
        assert_eq!(native.thread_name.as_deref(), Some(name));
    }
}

#[test]
fn null_default_schema_or_native_class_remains_unknown_and_does_not_borrow_the_request_schema() {
    let row = row_with(1, "Query", true, &[("db", Value::NULL), ("thread_name", Value::NULL)]);
    let point = decode(&row, "shop", true).unwrap().unwrap();
    assert_eq!(point.database.as_deref(), Some("shop"));
    let Some(SessionNativeContext::Mysql(native)) = point.session_context.as_deref().unwrap().native.as_deref() else {
        panic!()
    };
    assert!(native.default_database.is_none() && native.thread_name.is_none());
    assert_eq!(native.request_schema.as_deref(), Some("shop"));
    assert!(!native.is_client_thread(point.command.as_deref()));
}

#[tokio::test]
async fn selected_default_schema_frame_refuses_a_foreign_or_null_native_default_without_extra_reads() {
    for db in [Value::from("foreign"), Value::NULL] {
        let mut reader = Read::with(vec![Ok(consumers(true)), Ok(vec![row_with(1, "Query", true, &[("db", db)])])]);
        let error = observe(&mut reader, &metadata(), &CollectionBudget::default()).await.err().unwrap();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)));
        assert_eq!(reader.queries.len(), 2);
    }
}
