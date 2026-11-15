use super::super::tests::{self as support, CHECK_AFTER, FINAL_CONTEXT, HEADER_AFTER, HEADER_BEFORE, SQL_TEXT};
use super::*;
use crate::{
    activity::tests::details,
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
};

fn task_row(address: &str, context: i64) -> Rec {
    replace(
        &support::page_wait(42, "CXCONSUMER", ""),
        &[
            ("task_address", Val::Text(address.into())),
            ("task_session_id", Val::Int(42)),
            ("task_request_id", Val::Int(0)),
            ("task_execution_context_id", Val::Int(context)),
            ("task_wait_type", Val::Text("PAGELATCH_EX".into())),
            ("task_wait_duration_ms", Val::Int(9007199254740993)),
            ("task_resource_description", Val::Text("7:1:4144".into())),
            ("task_resource_address", Val::Text("00000000DDDD0001".into())),
            ("task_blocking_address", Val::Text("00000000AAAA0001".into())),
            ("task_blocking_session_id", Val::Int(-4)),
            ("task_blocking_execution_context_id", Val::Null),
        ],
    )
}

fn checked(ordinal: usize, row: &Rec) -> Rec {
    let participant = support::participant(ordinal, row);
    let mut fields = participant.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    for (output, input) in [
        ("checked_task_address", "task_address"),
        ("checked_task_session_id", "task_session_id"),
        ("checked_task_request_id", "task_request_id"),
        ("checked_task_execution_context_id", "task_execution_context_id"),
        ("checked_task_wait_type", "task_wait_type"),
        ("checked_task_resource", "task_resource_description"),
    ] {
        fields.push((output, super::super::super::rows::value(row, input).unwrap().clone()));
    }
    Rec::of(&fields)
}

fn replies(rows: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    let checks = rows.iter().enumerate().map(|(index, row)| checked(index, row)).collect::<Vec<_>>();
    let mut replies = support::successful_replies(rows);
    replies[2] = Ok(checks.clone());
    replies[HEADER_BEFORE] = Ok(vec![support::header(0, 4144)]);
    replies[HEADER_AFTER] = Ok(vec![support::header(0, 4144)]);
    replies[SQL_TEXT] = Ok(vec![details(42, "SELECT * FROM dbo.orders WHERE value=N'private-task-canary'")]);
    replies[CHECK_AFTER] = Ok(checks);
    replies
}

#[tokio::test]
async fn parallel_task_waiters_are_preserved_while_their_coordinator_waits_elsewhere_and_sql_is_read_once() {
    let mut native = support::reader(replies(vec![task_row("0000000000000101", 1), task_row("0000000000000102", 2)]));
    let capture = native.capture_page_wait_targets().await.unwrap();
    assert!(capture.snapshot.is_valid());
    assert_eq!(capture.snapshot.observations.len(), 2);
    assert_ne!(capture.snapshot.observations[0].key().unwrap(), capture.snapshot.observations[1].key().unwrap());
    for row in &capture.snapshot.observations {
        assert_eq!(row.request.wait.as_ref().unwrap().native_type.as_deref(), Some("CXCONSUMER"));
        assert!(row.request.wait.as_ref().unwrap().identified_resources.is_empty());
        assert_eq!(row.observed_wait().native_type, Some("PAGELATCH_EX"));
        assert_eq!(row.observed_wait().elapsed_ms, Some("9007199254740993"));
        assert_eq!(row.task_wait.as_ref().unwrap().blocking_session_id, Some(-4));
        assert_eq!(row.task_wait.as_ref().unwrap().blocking_task_address.as_deref(), Some("00000000AAAA0001"));
        assert_eq!(row.before_check.as_ref().unwrap().wait, PageWaitRelation::OriginalObserved);
        assert!(row.statement_identity.is_some());
        assert!(row.request.statement.is_available());
    }
    assert_eq!(native.query_count(), 10);
    assert_eq!(native.io.calls[SQL_TEXT].limit, 1, "parallel tasks share the exact request SQL payload read");
    assert_eq!(native.io.calls[HEADER_BEFORE].limit, 1);
    assert!(!serde_json::to_string(&capture.snapshot).unwrap().contains("private-task-canary"));
    assert!(native.io.calls[1].sql.contains("task.request_id"));
    assert!(!native.io.calls[1].sql.contains("ORDER BY"));
    assert!(!native.io.calls[1].sql.contains("dm_exec_sql_text"));
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_reused_or_ended_task_tuple_withholds_sql_association_but_retains_its_original_raw_wait() {
    for (field, value, relation) in [
        ("checked_task_address", Val::Null, PageWaitRelation::NoLongerObserved),
        ("checked_task_execution_context_id", Val::Int(99), PageWaitRelation::Unqualified),
        ("checked_task_resource", Val::Text("7:1:9999".into()), PageWaitRelation::Different),
        ("checked_request_generation", Val::Text("2020-01-01T07:59:00".into()), PageWaitRelation::Unqualified),
    ] {
        let row = task_row("0000000000000101", 1);
        let mut replies = replies(vec![row.clone()]);
        replies[CHECK_AFTER] = Ok(vec![replace(&checked(0, &row), &[(field, value)])]);
        let capture = support::reader(replies).capture_page_wait_targets().await.unwrap();
        let page = &capture.snapshot.observations[0];
        assert_eq!(page.after_check.as_ref().unwrap().wait, relation);
        assert_eq!(page.task_wait.as_ref().unwrap().resource.native.id, "7:1:4144");
        assert_eq!(page.task_wait.as_ref().unwrap().wait_duration_ms.as_deref(), Some("9007199254740993"));
        assert!(page.target.is_some(), "current page mapping is independent of an ended task");
        assert!(page.statement_identity.is_none());
        assert!(page.plan_identity.is_none());
        assert!(!page.request.statement.is_available());
    }
}

#[tokio::test]
async fn conflicting_statement_locators_on_one_request_are_not_borrowed_between_parallel_tasks() {
    let first = task_row("0000000000000101", 1);
    let second = replace(&task_row("0000000000000102", 2), &[("sql_handle", Val::Text("EF".repeat(64)))]);
    let mut replies = replies(vec![first, second]);
    let _ = replies.remove(SQL_TEXT);
    let mut native = support::reader(replies);
    let capture = native.capture_page_wait_targets().await.unwrap();
    for row in &capture.snapshot.observations {
        assert_eq!(row.before_check.as_ref().unwrap().request, PageWaitRequestState::Unverified);
        assert!(row.statement_identity.is_none());
        assert!(!row.request.statement.is_available());
    }
    assert!(native.io.calls.iter().all(|call| !call.sql.contains("dm_exec_sql_text")));
    assert_eq!(native.query_count(), 9);
}

#[tokio::test]
async fn unavailable_task_sql_preserves_the_native_payload_reason_instead_of_relabeling_it_missing() {
    let mut replies = replies(vec![task_row("0000000000000101", 1)]);
    replies[SQL_TEXT] =
        Ok(vec![replace(&details(42, ""), &[("source_bytes", Val::Int(200_000)), ("payload", Val::Null)])]);
    let capture = support::reader(replies).capture_page_wait_targets().await.unwrap();
    let row = &capture.snapshot.observations[0];
    assert!(matches!(row.request.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }));
    assert!(row.statement_identity.is_none());
    assert!(row.plan_identity.is_none());
    assert!(row.target.is_some());
}

#[tokio::test]
async fn ambiguous_native_task_rows_or_mismatched_request_tuples_stop_before_any_header_lookup() {
    let original = task_row("0000000000000101", 1);
    for rows in
        [vec![original.clone(), original.clone()], vec![replace(&original, &[("task_request_id", Val::Int(8))])]]
    {
        let mut native = support::reader(vec![Ok(vec![support::context()]), Ok(rows)]);
        let error = native.capture_page_wait_targets().await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
        assert_eq!(native.query_count(), 2);
        assert!(native.io.calls.iter().all(|call| !call.sql.contains("sys.dm_db_page_info(")));
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn task_cancellation_limits_source_fences_and_unconfirmed_close_never_publish_fresh_rows() {
    for stop in
        [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::ByteLimit, CollectionStop::QueryLimit]
    {
        let mut native = support::reader(vec![
            Ok(vec![support::context()]),
            Ok(vec![task_row("0000000000000101", 1)]),
            Err(ReadError::Stopped(stop)),
        ]);
        let (snapshot, coverage) = crate::page_waits::collect_from(&mut native, false).await;
        assert!(snapshot.is_none());
        assert_eq!(native.query_count(), 3);
        assert_eq!(native.io.closed, 1);
        assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Unavailable);
    }
    let mut changed = replies(vec![task_row("0000000000000101", 1)]);
    changed[FINAL_CONTEXT] =
        Ok(vec![replace(&support::context(), &[("physical_incarnation", Val::Text("new-native-authority".into()))])]);
    let error = support::reader(changed).capture_page_wait_targets().await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    let mut io = FakeRead::new(replies(vec![task_row("0000000000000101", 1)]));
    io.shutdown = SourceShutdown::Unconfirmed;
    let mut native = support::reader_with_native(io);
    let (snapshot, _) = crate::page_waits::collect_from(&mut native, false).await;
    assert!(snapshot.is_none());
    assert_eq!(native.io.closed, 1);
}

#[tokio::test]
async fn a_shared_tempdb_task_target_never_uses_the_application_database_header_function() {
    let row = replace(&task_row("0000000000000101", 1), &[("task_resource_description", Val::Text("2:1:1".into()))]);
    let mut native = support::reader(vec![
        Ok(vec![support::context()]),
        Ok(vec![row]),
        Ok(vec![details(42, "SELECT 1")]),
        Ok(vec![support::context()]),
    ]);
    let capture = native.capture_page_wait_targets().await.unwrap();
    assert_eq!(capture.snapshot.observations[0].resolution, PageWaitResolution::OutsideDatabase);
    assert!(capture.snapshot.observations[0].target.is_none());
    assert_eq!(capture.snapshot.observations[0].resource.native.id, "2:1:1");
    assert_eq!(
        capture.snapshot.observations[0].observed_address,
        Some(PageWaitAddress { database_id: 2, file_id: 1, page_id: 1 })
    );
    assert_eq!(capture.snapshot.observations[0].request.database.as_deref(), Some("shop"));
    assert!(native.io.calls.iter().all(|call| !call.sql.contains("sys.dm_db_page_info(")));
    assert_eq!(native.query_count(), 4);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unverified_task_visibility_keeps_readable_coordinators_and_empty_frames_explicitly_partial() {
    let coordinator =
        replace(&support::page_wait(42, "PAGELATCH_EX", "7:1:4144"), &[("task_visibility", Val::Bool(false))]);
    let mut native = support::reader(support::successful_replies(vec![coordinator]));
    let captured = native.capture_page_wait_targets().await.unwrap();
    assert!(captured.task_visibility_unavailable());
    assert_eq!(captured.snapshot.observations.len(), 1);
    assert!(captured.snapshot.observations[0].task_wait.is_none());
    assert!(captured.snapshot.observations[0].target.is_some());
    assert!(native.io.calls[1].sql.contains("IF @task_visibility=1"));
    assert!(!native.io.calls[2].sql.contains("sys.dm_os_waiting_tasks"));
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    for present in [true, false] {
        let replies = if present {
            support::successful_replies(vec![replace(
                &support::page_wait(42, "PAGELATCH_EX", "7:1:4144"),
                &[("task_visibility", Val::Bool(false))],
            )])
        } else {
            let empty = replace(&support::context(), &[("task_visibility", Val::Bool(false))]);
            vec![Ok(vec![support::context()]), Ok(vec![empty]), Ok(vec![support::context()])]
        };
        let mut native = support::reader(replies);
        let (snapshot, coverage) = crate::page_waits::collect_from(&mut native, false).await;
        assert_eq!(snapshot.unwrap().observations.len(), usize::from(present));
        let outcome = &coverage.outcomes()[0];
        assert_eq!(outcome.state, culprit_core::SourceState::Partial);
        assert_eq!(
            outcome.reason_message,
            Some(culprit_core::i18n::Message::paragraph(vec![
                culprit_core::message!("provider.mssql.page_wait_targets.partial"),
                culprit_core::message!("provider.mssql.page_wait_targets.task_visibility_unavailable"),
            ]))
        );
        assert_eq!(native.query_count(), if present { 10 } else { 3 });
        assert_eq!(native.io.closed, 1);
    }
}

#[tokio::test]
async fn task_population_sentinel_preserves_only_the_admitted_point_without_querying_overflow_requests() {
    let rows = vec![task_row("0000000000000101", 1), task_row("0000000000000102", 2)];
    let mut replies = replies(rows.clone());
    replies[2] = Ok(vec![checked(0, &rows[0])]);
    replies[CHECK_AFTER] = Ok(vec![checked(0, &rows[0])]);
    let io = FakeRead::new(replies);
    let mut native = ActivityReader::new(
        io,
        crate::activity::ReadOptions {
            budget: CollectionBudget { max_snapshot_rows: 1, ..CollectionBudget::default() },
            scope: ActivityScope::CurrentDatabase,
            identity: crate::activity::IdentityDetail::AzureDatabase,
        },
        "fixture.invalid".into(),
        1433,
    );
    let capture = native.capture_page_wait_targets().await.unwrap();
    assert!(capture.limited);
    assert_eq!(capture.snapshot.observations.len(), 1);
    assert_eq!(capture.snapshot.observations[0].task_wait.as_ref().unwrap().execution_context_id, 1);
    assert_eq!(native.io.calls[1].limit, 2);
    assert_eq!(native.io.calls[2].limit, 1);
    assert_eq!(native.io.calls[SQL_TEXT].limit, 1);
}
