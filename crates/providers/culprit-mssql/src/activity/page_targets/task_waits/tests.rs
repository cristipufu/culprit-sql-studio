use super::super::tests::{self as support, CHECK_AFTER, FINAL_CONTEXT, HEADER_AFTER, HEADER_BEFORE, SQL_TEXT};
use super::*;
use crate::{
    activity::tests::details,
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
};

pub(in crate::activity::page_targets) fn task_row(address: &str, context: i64) -> Rec {
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

pub(in crate::activity::page_targets) fn buffer_resource(page: u32, task: &str) -> String {
    format!(
        "7:1:{page} (LATCH 0x0000000BF6759098: Class: BUFFER KP: 0 SH: 0 UP: 0 EX: 1 DT: 0 Sublatch: 0 HasWaiters: 1 Task: 0x{task} AnyReleasor: 0)"
    )
}

pub(in crate::activity::page_targets) fn checked(ordinal: usize, row: &Rec) -> Rec {
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

pub(in crate::activity::page_targets) fn replies(rows: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
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
        ("checked_task_resource", Val::Text(buffer_resource(9999, "0000000B96434108")), PageWaitRelation::Different),
        ("checked_task_wait_type", Val::Text("PAGELATCH_SH".into()), PageWaitRelation::Different),
        ("checked_task_wait_type", Val::Text("PAGEIOLATCH_EX".into()), PageWaitRelation::Different),
        (
            "checked_task_resource",
            Val::Text("7:1:4144 (unsupported diagnostic data)".into()),
            PageWaitRelation::Unqualified,
        ),
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
    for (field, changed) in [
        ("sql_handle", Val::Text("EF".repeat(64))),
        ("statement_start", Val::Int(8)),
        ("statement_end", Val::Int(12)),
        ("sql_handle", Val::Null),
    ] {
        let first = task_row("0000000000000101", 1);
        let second = replace(&task_row("0000000000000102", 2), &[(field, changed)]);
        let mut replies = replies(vec![first, second]);
        let _ = replies.remove(SQL_TEXT);
        let mut native = support::reader(replies);
        let capture = native.capture_page_wait_targets().await.unwrap();
        assert_eq!(capture.snapshot.observations.len(), 2);
        for row in &capture.snapshot.observations {
            for check in [&row.before_check, &row.after_check] {
                let check = check.as_ref().unwrap();
                assert_eq!(check.request, PageWaitRequestState::Unverified, "{field}");
                assert_eq!(check.wait, PageWaitRelation::Unqualified, "{field}");
            }
            assert!(row.request.statement_identity.is_none(), "{field}");
            assert!(row.statement_identity.is_none() && row.plan_identity.is_none(), "{field}");
            assert!(!row.request.statement.is_available(), "{field}");
            assert!(
                row.task_wait.is_some() && row.target.is_some(),
                "raw waits and independent mapping remain available"
            );
        }
        assert!(native.io.calls.iter().all(|call| !call.sql.contains("dm_exec_sql_text")));
        assert_eq!(native.query_count(), 9);
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
        assert_eq!(native.io.closed, 1);
    }
}

#[test]
fn conflicting_request_database_locators_invalidate_both_native_statement_representations() {
    let first = task_row("0000000000000101", 1);
    let second = replace(
        &task_row("0000000000000102", 2),
        &[
            ("request_database_id", Val::Int(8)),
            ("scope_basis", Val::Int(5)),
            ("session_database", Val::Text("other".into())),
        ],
    );
    let mut records =
        [first, second].iter().map(|row| crate::activity::rows::record(row).unwrap().unwrap()).collect::<Vec<_>>();
    assert!(records.iter().all(|record| record.current_locator.is_some() && record.facts.statement_identity.is_some()));
    withhold_conflicting_locators(&mut records);
    for record in records {
        assert!(record.current_locator.is_none());
        assert!(record.facts.statement_identity.is_none());
        assert!(matches!(record.current_sql, NativeStatement::Unavailable(PayloadUnavailable::Missing)));
    }
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

#[tokio::test]
async fn changing_buffer_diagnostics_preserves_the_exact_task_page_and_original_raw_observation() {
    let original = buffer_resource(4144, "0000000B96434108");
    let current = buffer_resource(4144, "0000000B96434109");
    let row = replace(&task_row("0000000000000101", 1), &[("task_resource_description", Val::Text(original.clone()))]);
    let mut supplied = replies(vec![row.clone()]);
    supplied[CHECK_AFTER] = Ok(vec![replace(&checked(0, &row), &[("checked_task_resource", Val::Text(current))])]);
    let mut native = support::reader(supplied);
    let captured = native.capture_page_wait_targets().await.unwrap();
    let page = &captured.snapshot.observations[0];
    assert_eq!(page.before_check.as_ref().unwrap().wait, PageWaitRelation::OriginalObserved);
    assert_eq!(page.after_check.as_ref().unwrap().wait, PageWaitRelation::OriginalObserved);
    assert_eq!(page.task_wait.as_ref().unwrap().resource.native.id, original);
    assert_eq!(page.resource.native.id, original);
    assert_eq!(page.observed_address, Some(PageWaitAddress { database_id: 7, file_id: 1, page_id: 4144 }));
    assert!(page.statement_identity.is_some() && page.request.statement.is_available());
    assert!(page.target.is_some());
    assert_eq!(native.query_count(), 10);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    assert_eq!(native.io.closed, 1);
}

#[test]
fn unsupported_original_page_or_native_provenance_withholds_relation_instead_of_claiming_a_different_wait() {
    let row = task_row("0000000000000101", 1);
    let original = crate::activity::rows::record(&row).unwrap().unwrap();
    let task = decode(&row, &crate::read::text(&row, "observed_at").unwrap()).unwrap().unwrap();
    let checked = super::super::participants::CheckedParticipant::decode(&checked(0, &row), true).unwrap();
    for defect in ["unknown-address", "unknown-diagnostics", "different-source", "extra-provenance", "different-kind"] {
        let mut unqualified = task.clone();
        match defect {
            "unknown-address" => unqualified.resource.native.id = "not-a-native-page-address".into(),
            "unknown-diagnostics" => unqualified.resource.native.id = "7:1:4144 (unsupported diagnostic data)".into(),
            "different-source" => {
                unqualified
                    .resource
                    .native
                    .qualifiers
                    .insert("source".into(), "sys.dm_exec_requests.wait_resource".into());
            }
            "extra-provenance" => {
                unqualified.resource.native.qualifiers.insert("other-reader".into(), "unverified".into());
            }
            _ => unqualified.resource.kind = WaitResourceKind::PageIo,
        }
        let recheck = checked.recheck_observation(&original.facts, &unqualified.resource, Some(&unqualified));
        assert_eq!(recheck.request, PageWaitRequestState::SameStatement, "{defect}");
        assert_eq!(recheck.wait, PageWaitRelation::Unqualified, "{defect}");
    }
}

#[tokio::test]
async fn one_population_cannot_choose_between_duplicate_task_slots_with_different_resource_diagnostics() {
    let original = task_row("0000000000000101", 1);
    for page in [4144, 9999] {
        let duplicate =
            replace(&original, &[("task_resource_description", Val::Text(buffer_resource(page, "0000000B96434108")))]);
        let mut native = support::reader(vec![Ok(vec![support::context()]), Ok(vec![original.clone(), duplicate])]);
        let error = native.capture_page_wait_targets().await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
        assert_eq!(native.query_count(), 2);
        assert!(native.io.calls.iter().all(|call| !call.sql.contains("sys.dm_db_page_info(")));
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
        assert_eq!(native.io.closed, 1);
    }
}
