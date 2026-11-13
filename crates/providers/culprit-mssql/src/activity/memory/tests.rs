use super::*;
use crate::{
    activity::{
        ReadOptions,
        tests::{details, metadata},
    },
    read::ReadResult,
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn context() -> Rec {
    metadata(0)
}
pub(crate) fn columns() -> Vec<Rec> {
    fields::columns()
}
pub(crate) fn row() -> Rec {
    let mut values = metadata(42).columns().map(|(key, value)| (key.to_owned(), value.clone())).collect::<Vec<_>>();
    for (name, value) in [
        ("grant_session_id", Val::Int(42)),
        ("grant_request_id", Val::Int(0)),
        ("grant_requested_without_offset", Val::Text("2020-01-01T07:58:01".into())),
        ("grant_granted_without_offset", Val::Text("2020-01-01T07:58:01.003".into())),
        ("grant_sql_handle", Val::Text("AB".repeat(64))),
        ("grant_plan_handle", Val::Text("CD".repeat(64))),
        ("request_plan_handle", Val::Text("CD".repeat(64))),
        ("grant_pool_id", Val::Int(2)),
        ("grant_group_id", Val::Int(7)),
        ("grant_semaphore_id", Val::Int(0)),
        ("grant_requested_memory_kb", Val::Int(9_007_199_254_740_993)),
        ("grant_required_memory_kb", Val::Int(64)),
        ("grant_granted_memory_kb", Val::Int(65_536)),
        ("grant_used_memory_kb", Val::Int(0)),
        ("grant_max_used_memory_kb", Val::Int(8192)),
        ("grant_ideal_memory_kb", Val::Null),
        ("grant_wait_time_ms", Val::Null),
        ("grant_timeout_sec", Val::Int(25)),
        ("grant_dop", Val::Int(1)),
        ("grant_reserved_worker_count", Val::Int(1)),
        ("grant_used_worker_count", Val::Int(1)),
        ("grant_max_used_worker_count", Val::Int(1)),
        ("grant_queue_id", Val::Null),
        ("grant_wait_order", Val::Null),
        ("grant_is_next_candidate", Val::Null),
        ("grant_is_small", Val::Int(0)),
        ("grant_query_cost", Val::Float(1.25)),
    ] {
        values.push((name.into(), value));
    }
    Rec::of(&values.iter().map(|(key, value)| (key.as_str(), value.clone())).collect::<Vec<_>>())
}
pub(crate) fn reader(replies: Vec<ReadResult<Vec<Rec>>>) -> ActivityReader<FakeRead> {
    reader_with_identity(replies, IdentityDetail::Endpoint)
}
pub(crate) fn reader_with_identity(
    replies: Vec<ReadResult<Vec<Rec>>>,
    identity: IdentityDetail,
) -> ActivityReader<FakeRead> {
    ActivityReader::new(
        FakeRead::new(replies),
        ReadOptions { budget: CollectionBudget::default(), scope: ActivityScope::CurrentDatabase, identity },
        "fixture.invalid".into(),
        1433,
    )
}
pub(crate) fn successful() -> ActivityReader<FakeRead> {
    reader(vec![
        Ok(vec![context()]),
        Ok(columns()),
        Ok(vec![row()]),
        Ok(vec![details(42, "SELECT * FROM dbo.orders WHERE id=42")]),
        Ok(vec![context()]),
    ])
}
pub(crate) fn set_shutdown(reader: &mut ActivityReader<FakeRead>, shutdown: SourceShutdown) {
    reader.io.shutdown = shutdown;
}
pub(crate) fn closed(reader: &ActivityReader<FakeRead>) -> u32 {
    reader.io.closed
}

#[tokio::test]
async fn native_grant_probe_reuses_request_generations_exact_counters_and_guarded_statement_enrichment() {
    let mut reader = successful();
    let captured = reader.capture_memory().await.unwrap();
    assert!(captured.complete_scope);
    let [grant] = captured.observations.as_slice() else { panic!() };
    assert_eq!(grant.state, ExecutionGrantState::Granted);
    assert_eq!(grant.request_association, RequestGrantAssociation::GenerationChecked);
    assert_eq!(grant.requested_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(grant.used_kibibytes.as_deref(), Some("0"));
    assert_eq!(grant.ideal_kibibytes, None);
    assert_eq!(grant.native_properties["plan_handle"], format!("0x{}", "CD".repeat(64)));
    assert_eq!(grant.statement_relationship, GrantStatementRelationship::ObservedCurrentStatement);
    assert_eq!(grant.request.as_ref().unwrap().key.session.id, "42");
    assert!(
        matches!(&grant.request.as_ref().unwrap().statement,DefinitionPayload::Available{text,..} if !text.contains("42"))
    );
    assert!(reader.verify_memory_context(&captured.context).await.unwrap());
    assert_eq!(reader.query_count(), 5);
    assert!(reader.io.calls.iter().all(|call| call.sql.contains("OPTION(MAXDOP 1, LOOP JOIN)")));
    let probe = &reader.io.calls[2].sql;
    for disallowed in
        ["ORDER BY", "COUNT(", "COUNT_BIG(", "SUM(", "GROUP BY", "sys.dm_exec_query_plan", "sys.dm_os_memory_objects"]
    {
        assert!(!probe.contains(disallowed), "{disallowed}");
    }
    assert!(probe.contains("r.request_id=mg.request_id"));
    assert!(probe.contains("r.database_id=DB_ID() OR NULLIF(r.database_id,0) IS NULL"));
    assert_eq!(reader.io.calls[2].limit, 501);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn native_nulls_and_version_unavailable_fields_preserve_unknowns_without_inventing_pool_identity() {
    let mut native = row();
    native = replace(
        &native,
        &[
            ("grant_pool_id", Val::Null),
            ("grant_group_id", Val::Null),
            ("grant_granted_without_offset", Val::Null),
            ("grant_granted_memory_kb", Val::Null),
            ("grant_used_memory_kb", Val::Null),
            ("grant_wait_time_ms", Val::Int(40_000)),
        ],
    );
    let columns = columns().into_iter().filter(|row| !row.string("name").contains("worker_count")).collect();
    let mut reader = reader(vec![Ok(vec![context()]), Ok(columns), Ok(vec![native])]);
    let frame = reader.observe_memory().await.unwrap();
    assert!(frame.complete_scope, "unavailable optional columns do not imply missing request rows");
    let rows = frame.into_observations().unwrap();
    let grant = &rows[0];
    assert_eq!(grant.state, ExecutionGrantState::Waiting);
    assert!(grant.resource.is_none() && grant.workload_group.is_none());
    assert_eq!(grant.granted_kibibytes, None);
    assert_eq!(grant.used_kibibytes, None);
    assert_eq!(grant.wait_ms.as_deref(), Some("40000"));
    assert!(grant.native_null_columns.contains(&"pool_id".into()));
    assert_eq!(grant.native_properties["resource_semaphore_id"], "0");
    assert!(!grant.native_properties.contains_key("pool_id"));
    assert!(!grant.available_columns.contains(&"used_worker_count".into()));
    assert_eq!(reader.query_count(), 3);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn reused_ids_old_handles_and_missing_requests_preserve_unassigned_native_grants() {
    for (changes, expected) in [
        (
            vec![("request_generation", Val::Text("2020-01-01T07:59:00".into()))],
            RequestGrantAssociation::ChangedRequest,
        ),
        (
            vec![("session_generation", Val::Text("2020-01-01T07:59:00".into()))],
            RequestGrantAssociation::ChangedRequest,
        ),
        (vec![("request_plan_handle", Val::Text("EF".repeat(64)))], RequestGrantAssociation::ChangedRequest),
        (vec![("request_id", Val::Null)], RequestGrantAssociation::MissingRequest),
        (vec![("grant_plan_handle", Val::Null)], RequestGrantAssociation::InsufficientEvidence),
        (vec![("scope_basis", Val::Int(5))], RequestGrantAssociation::InsufficientEvidence),
    ] {
        let mut reader = reader(vec![Ok(vec![context()]), Ok(columns()), Ok(vec![replace(&row(), &changes)])]);
        let frame = reader.observe_memory().await.unwrap();
        assert!(!frame.complete_scope);
        let rows = frame.into_observations().unwrap();
        assert_eq!(rows[0].request_association, expected);
        assert!(rows[0].request.is_none() && rows[0].scope.is_none());
        assert_eq!(rows[0].state, ExecutionGrantState::Granted);
        assert_eq!(rows[0].granted_kibibytes.as_deref(), Some("65536"));
        assert_eq!(reader.query_count(), 3, "unknown ownership never probes a previous batch or broadens the source");
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn different_native_resources_and_waiting_requests_do_not_establish_a_holder_waiter_causal_edge() {
    let other = replace(
        &row(),
        &[
            ("session_id", Val::Int(43)),
            ("grant_session_id", Val::Int(43)),
            ("grant_pool_id", Val::Int(3)),
            ("grant_granted_without_offset", Val::Null),
            ("grant_granted_memory_kb", Val::Null),
            ("grant_wait_time_ms", Val::Int(30_000)),
        ],
    );
    let mut reader = reader(vec![Ok(vec![context()]), Ok(columns()), Ok(vec![row(), other])]);
    let frame = reader.observe_memory().await.unwrap();
    let rows = frame.into_observations().unwrap();
    assert_eq!(rows[0].state, ExecutionGrantState::Granted);
    assert_eq!(rows[1].state, ExecutionGrantState::Waiting);
    assert_ne!(rows[0].resource, rows[1].resource);
    assert!(rows.iter().all(|row| row.statement_relationship == GrantStatementRelationship::Unresolved));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn hidden_visibility_or_required_metadata_never_reads_grants_or_reports_empty_success() {
    let denied = replace(&context(), &[("database_wide", Val::Int(0))]);
    let mut denied = reader(vec![Ok(vec![denied])]);
    assert_eq!(denied.observe_memory().await.err().unwrap().code, culprit_core::ErrorCode::PermissionDenied);
    assert_eq!(denied.query_count(), 1);
    let mut hidden = reader(vec![Ok(vec![context()]), Ok(vec![])]);
    assert_eq!(
        CollectionStop::from_error(&hidden.observe_memory().await.err().unwrap()),
        CollectionStop::SourceUnavailable
    );
    assert_eq!(hidden.query_count(), 2);
}

#[tokio::test]
async fn empty_visible_native_grants_and_unavailable_optional_sql_keep_different_meanings() {
    let mut values = context().columns().map(|(key, value)| (key.to_owned(), value.clone())).collect::<Vec<_>>();
    values.push(("grant_session_id".into(), Val::Null));
    let empty = Rec::of(&values.iter().map(|(key, value)| (key.as_str(), value.clone())).collect::<Vec<_>>());
    let mut empty = reader(vec![Ok(vec![context()]), Ok(columns()), Ok(vec![empty])]);
    let captured = empty.capture_memory().await.unwrap();
    assert!(captured.complete_scope && captured.observations.is_empty());
    assert_eq!(empty.query_count(), 3, "no SQL enrichment is requested for native absence");
    let mut denied = reader(vec![
        Ok(vec![context()]),
        Ok(columns()),
        Ok(vec![row()]),
        Err(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture SQL denied").into()),
    ]);
    let captured = denied.capture_memory().await.unwrap();
    assert!(captured.complete_scope);
    assert_eq!(captured.observations.len(), 1);
    let grant = &captured.observations[0];
    assert_eq!(grant.request_association, RequestGrantAssociation::GenerationChecked);
    assert_eq!(grant.statement_relationship, GrantStatementRelationship::Unresolved);
    assert!(matches!(
        grant.request.as_ref().unwrap().statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::SourceUnavailable }
    ));
    assert_eq!(denied.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn overflow_terminal_enrichment_and_retired_calls_never_reopen_or_verify_native_work() {
    let mut overflow = reader(vec![Ok(vec![context()]), Ok(columns()), Ok(vec![row(), row()])]);
    overflow.options.budget.max_snapshot_rows = 1;
    assert_eq!(CollectionStop::from_error(&overflow.observe_memory().await.err().unwrap()), CollectionStop::RowLimit);
    assert_eq!(overflow.query_count(), 3);
    for code in [
        culprit_core::ErrorCode::Timeout,
        culprit_core::ErrorCode::ConnectionFailed,
        culprit_core::ErrorCode::AuthFailed,
    ] {
        let mut reader = reader(vec![
            Ok(vec![context()]),
            Ok(columns()),
            Ok(vec![row()]),
            Err(culprit_core::Error::new(code, "fixture enrichment failed").into()),
        ]);
        assert_eq!(reader.capture_memory().await.err().unwrap().code, code);
        assert!(reader.capture_memory().await.is_err());
        assert_eq!(reader.query_count(), 4);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let mut retired = ActivityReader::new(
        FakeRead::new(vec![]).pause_on_query(1),
        ReadOptions {
            budget: CollectionBudget::default(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::Endpoint,
        },
        "fixture.invalid".into(),
        1433,
    );
    assert!(tokio::time::timeout(std::time::Duration::from_millis(1), retired.observe_memory()).await.is_err());
    assert!(retired.observe_memory().await.is_err());
    assert_eq!(retired.query_count(), 1);
    assert_eq!(retired.close().await, SourceShutdown::Quiescent);
}
