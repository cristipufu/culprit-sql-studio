use super::*;
use crate::{
    activity::{
        IdentityDetail, ReadOptions,
        page_targets::tests::participant,
        tests::{details, metadata},
    },
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn context() -> Rec {
    let base = metadata(42);
    let mut values = base.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    values.push(("task_space_permission", Val::Int(1)));
    replace(&Rec::of(&values), &[("session_id", Val::Null)])
}
pub(crate) fn task(execution: i64) -> Rec {
    let base = metadata(42);
    let mut values = base.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    values.extend([
        ("task_session_id", Val::Int(42)),
        ("task_request_id", Val::Int(0)),
        ("task_execution_context_id", Val::Int(execution)),
        ("task_database_id", Val::Int(2)),
        ("user_allocated_pages", Val::Int(i64::MAX)),
        ("user_deallocated_pages", Val::Int(0)),
        ("internal_allocated_pages", Val::Null),
        ("internal_deallocated_pages", Val::Int(9007199254740993)),
    ]);
    Rec::of(&values)
}
pub(crate) fn reader(replies: Vec<ReadResult<Vec<Rec>>>) -> ActivityReader<FakeRead> {
    ActivityReader::new(
        FakeRead::new(replies),
        ReadOptions {
            budget: crate::task_space::bounded_budget(&CollectionBudget::default()),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::AzureDatabase,
        },
        "fixture.invalid".into(),
        1433,
    )
}
pub(crate) fn replies(tasks: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    let check = participant(0, &task(0));
    vec![
        Ok(vec![context()]),
        Ok(tasks),
        Ok(vec![check.clone()]),
        Ok(vec![details(42, "SELECT value FROM dbo.orders WHERE id=42")]),
        Ok(vec![check]),
        Ok(vec![context()]),
    ]
}

#[tokio::test]
async fn parallel_tasks_share_one_guarded_request_read_without_summing_allocations_or_session_totals() {
    let mut reader = reader(replies(vec![task(0), task(1)]));
    let captured = reader.capture_task_space().await.unwrap();
    assert!(!captured.limited && captured.context_complete);
    let rows = &captured.snapshot.observations;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].user_allocated_pages.as_deref(), Some("9223372036854775807"));
    assert_eq!(rows[0].user_deallocated_pages.as_deref(), Some("0"));
    assert_eq!(rows[0].internal_allocated_pages, None);
    assert_eq!(rows[0].internal_deallocated_pages.as_deref(), Some("9007199254740993"));
    assert_eq!(rows[0].bytes_per_page, 8192);
    assert_ne!(rows[0].key().unwrap(), rows[1].key().unwrap());
    assert!(rows.iter().all(|row| row.request_context() == TaskRequestContext::CheckedAtBounds));
    assert!(rows.iter().all(|row|matches!(&row.request.as_ref().unwrap().statement,DefinitionPayload::Available{text,..}if!text.contains("42"))));
    assert_eq!(reader.query_count(), 6);
    assert_eq!(reader.io.calls[2].limit, 1);
    assert_eq!(reader.io.calls[3].limit, 1);
    assert_eq!(reader.io.calls[4].limit, 1);
    assert_eq!(reader.io.calls[1].limit, 201);
    let sql = &reader.io.calls[1].sql;
    for forbidden in
        ["ORDER BY", "GROUP BY", "SUM(", "sys.dm_db_session_space_usage", "sys.dm_exec_query_plan", "sys.dm_os_tasks"]
    {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
    assert!(sql.contains("su.session_id<>@@SPID"));
    assert!(reader.io.calls.iter().all(|call| call.sql.contains("LOOP JOIN")));
}

#[tokio::test]
async fn changed_missing_or_recycled_request_checks_keep_native_counts_without_stale_sql_or_handles() {
    for (field, value, expected) in [
        ("checked_session_generation", Val::Text("2020-01-01T07:30:00".into()), TaskRequestContext::Changed),
        ("checked_request_generation", Val::Text("2020-01-01T07:59:00".into()), TaskRequestContext::Changed),
        ("checked_begin", Val::Int(2), TaskRequestContext::Changed),
        ("checked_request_id", Val::Null, TaskRequestContext::Missing),
    ] {
        let mut sequence = replies(vec![task(0)]);
        sequence[4] = Ok(vec![replace(&participant(0, &task(0)), &[(field, value)])]);
        let captured = reader(sequence).capture_task_space().await.unwrap();
        let row = &captured.snapshot.observations[0];
        assert_eq!(row.request_context(), expected);
        assert!(!row.request.as_ref().unwrap().statement.is_available());
        assert!(row.statement_identity.is_none() && row.plan_identity.is_none());
        assert!(!captured.context_complete);
        assert_eq!(row.user_allocated_pages.as_deref(), Some("9223372036854775807"));
    }
}

#[tokio::test]
async fn strict_visibility_denial_precedes_task_sql_and_never_becomes_a_successful_empty_read() {
    let mut reader = reader(vec![Ok(vec![replace(&context(), &[("task_space_permission", Val::Int(0))])])]);
    assert_eq!(reader.capture_task_space().await.err().unwrap().code, culprit_core::ErrorCode::PermissionDenied);
    assert_eq!(reader.query_count(), 1);
    assert!(reader.io.calls.iter().all(|call| !call.sql.contains("FROM sys.dm_db_task_space_usage")));
    assert!(reader.io.calls[0].sql.contains("'Basic','S0','S1','ElasticPool','System'"));
}

#[tokio::test]
async fn native_rows_without_current_request_stay_unowned_without_sql_enrichment() {
    let native = replace(&task(0), &[("session_id", Val::Null), ("request_id", Val::Null)]);
    let mut reader = reader(vec![Ok(vec![context()]), Ok(vec![native]), Ok(vec![context()])]);
    let captured = reader.capture_task_space().await.unwrap();
    let row = &captured.snapshot.observations[0];
    assert!(row.request.is_none());
    assert_eq!(row.request_context(), TaskRequestContext::Unverified);
    assert!(!captured.context_complete);
    assert_eq!(reader.query_count(), 3);
}

#[tokio::test]
async fn task_limit_retains_a_bounded_partial_prefix_and_never_silently_truncates_to_empty() {
    let tasks = (0..=MAX_NATIVE_ROWS).map(|at| task(at as i64)).collect();
    let mut reader = reader(replies(tasks));
    let captured = reader.capture_task_space().await.unwrap();
    assert!(captured.limited);
    assert_eq!(captured.snapshot.observations.len(), MAX_NATIVE_ROWS);
    assert_eq!(reader.query_count(), 6);
    assert_eq!(reader.io.calls[1].limit, 201);
}

#[tokio::test]
async fn terminal_errors_do_not_trigger_later_check_enrichment_or_identity_queries() {
    for reason in [
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
    ] {
        for failed in [1, 2, 3, 4] {
            let mut sequence = replies(vec![task(0)]);
            sequence[failed] = Err(ReadError::Stopped(reason));
            let mut reader = reader(sequence);
            assert_eq!(CollectionStop::from_error(&reader.capture_task_space().await.err().unwrap()), reason);
            assert_eq!(reader.query_count(), failed as u32 + 1);
        }
    }
}

#[tokio::test]
async fn negative_counters_duplicate_task_tuples_and_changed_source_are_rejected() {
    for native in [vec![replace(&task(0), &[("user_allocated_pages", Val::Int(-1))])], vec![task(0), task(0)]] {
        assert!(reader(replies(native)).capture_task_space().await.is_err());
    }
    let mut sequence = replies(vec![task(0)]);
    sequence[5] = Ok(vec![replace(&context(), &[("physical_incarnation", Val::Text("replacement".into()))])]);
    assert_eq!(
        CollectionStop::from_error(&reader(sequence).capture_task_space().await.err().unwrap()),
        CollectionStop::SourceChanged
    );
}

#[tokio::test]
async fn a_request_detail_permission_gap_retains_native_counts_but_malformed_evidence_fails() {
    let mut sequence = replies(vec![task(0)]);
    assert!(sequence.remove(3).is_ok()); // A failed before check admits no guarded SQL query.
    sequence[2] = Err(ReadError::Source(culprit_core::Error::new(
        culprit_core::ErrorCode::PermissionDenied,
        "native permission failure",
    )));
    let captured = reader(sequence).capture_task_space().await.unwrap();
    assert_eq!(captured.context_error_code, Some(culprit_core::ErrorCode::PermissionDenied));
    assert!(!captured.context_complete);
    let row = &captured.snapshot.observations[0];
    assert_eq!(row.user_allocated_pages.as_deref(), Some("9223372036854775807"));
    assert!(!row.request.as_ref().unwrap().statement.is_available());
    assert!(row.statement_identity.is_none());
    let mut sequence = replies(vec![task(0)]);
    sequence[2] = Ok(vec![]);
    let mut reader = reader(sequence);
    assert!(reader.capture_task_space().await.is_err());
    assert_eq!(reader.query_count(), 3, "malformed proof is not an optional permission gap");
}
