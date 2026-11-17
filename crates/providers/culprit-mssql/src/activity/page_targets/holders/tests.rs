use super::*;
use crate::{
    activity::page_targets::tests::{context, header, participant, reader},
    activity::tests::{details, metadata},
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

fn task_row() -> Rec {
    replace(
        &task_waits::test_task_row("0000000000000101", 1),
        &[("task_blocking_session_id", Val::Int(43)), ("task_blocking_execution_context_id", Val::Int(0))],
    )
}

pub(crate) async fn focus() -> PageTaskHolderFocus {
    let row = task_row();
    let mut original = reader(task_waits::test_replies(vec![row.clone()]));
    let captured = original.capture_page_wait_targets().await.unwrap();
    assert_eq!(original.close().await, SourceShutdown::Quiescent);
    let page = &captured.snapshot.observations[0];
    let mut request =
        super::super::test_focus(&super::super::tests::page_wait(42, "PAGELATCH_EX", "7:1:4144")).participant;
    request.original = page.request.clone();
    let mut reference = request.parent.clone();
    reference.capture.observation_id = "original-task-page".into();
    reference.stream = TelemetryStream::OperationalSamples;
    reference.batch_id = batch_id(&reference.capture.observation_id, reference.stream).unwrap();
    reference.native_key = NativeRecordIdentity::Operational { source: page.source.clone(), key: page.key().unwrap() };
    let focus = PageTaskHolderFocus {
        participant: request,
        page: reference,
        address: page.observed_address.unwrap(),
        task: page.task_wait.clone().unwrap(),
        target: page.target.clone().unwrap(),
        original_observed_until: page.observed_until.clone(),
    };
    assert!(focus.is_valid());
    focus
}

fn merge(row: &Rec, extra: &Rec, additional: &[(&str, Val)]) -> Rec {
    let mut values = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    for (name, value) in extra.columns().map(|(name, value)| (name, value.clone())).chain(additional.iter().cloned()) {
        if let Some((_, prior)) = values.iter_mut().find(|(key, _)| *key == name) {
            *prior = value;
        } else {
            values.push((name, value));
        }
    }
    Rec::of(&values)
}

fn point(task: &Rec, owners: &[Rec]) -> Vec<Rec> {
    let mut rows = vec![merge(task, &task_waits::test_checked(0, task), &[("actor_user_process", Val::Bool(true))])];
    rows.extend(owners.iter().map(|owner| {
        merge(
            owner,
            &participant(1, owner),
            &[
                ("task_visibility", Val::Bool(true)),
                ("actor_user_process", Val::Bool(true)),
                ("holder_task_address", Val::Text("00000000AAAA0001".into())),
                ("holder_session_id", Val::Int(43)),
                ("holder_request_id", Val::Int(0)),
                ("holder_execution_context_id", Val::Int(0)),
            ],
        )
    }));
    rows
}

pub(crate) fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    let task = task_row();
    let owner = metadata(43);
    let mut replies = task_waits::test_replies(vec![task.clone()]);
    let healthy = replies[3].as_ref().unwrap().clone();
    replies[1] = Ok(point(&task, std::slice::from_ref(&owner)));
    replies.extend([
        Ok(vec![details(43, "INSERT dbo.orders(id) VALUES(7)")]),
        Ok(healthy),
        Ok(vec![header(0, 4144)]),
        Ok(point(&task, &[owner])),
        Ok(vec![context()]),
    ]);
    replies
}

#[tokio::test]
async fn exact_reported_task_holder_is_distinct_from_parallel_waiter_and_uses_one_owned_reader() {
    let focus = focus().await;
    let mut native = reader(replies());
    let captured = native.capture_page_task_holders(&focus).await.unwrap();
    assert!(focus.accepts_observation(&captured.context, &captured.observation));
    for point in [&captured.observation.before, &captured.observation.after] {
        assert_eq!(point.holder.relation, PageTaskHolderRelation::QualifiedRequest);
        let owner = point.holder.request.as_ref().unwrap();
        assert_eq!(owner.key.session.id, "43");
        assert_eq!(owner.key.request.as_ref().unwrap().id, "0");
        assert!(owner.statement.is_available());
    }
    let page = captured.observation.page.as_ref().unwrap();
    assert_eq!(page.request.key.session.id, "42");
    assert_eq!(page.request.wait.as_ref().unwrap().native_type.as_deref(), Some("CXCONSUMER"));
    assert_eq!(page.task_wait.as_ref().unwrap().wait_type, "PAGELATCH_EX");
    assert!(ObservationBounds::new(&page.observed_until, &captured.observation.after.observed_at).is_some());
    assert_eq!(page.target, Some(focus.target.clone()), "the first mapping remains its original native point");
    assert!(captured.observation.final_target.is_some());
    assert_eq!(native.query_count(), 15);
    let query = &native.io.calls[1];
    assert!(query.sql.contains("holder.task_address=CASE WHEN selected.reported_session>0"));
    assert!(query.sql.contains("r.session_id=a.session_id AND r.request_id=a.request_id"));
    assert!(!query.sql.contains("dm_exec_query_stats"));
    assert!(!query.sql.contains("dm_exec_query_plan"));
    assert!(matches!(&query.parameters[3],Parameter::Text(address) if address==&focus.task.task_address));
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    assert_eq!(native.io.closed, 1);
}

#[tokio::test]
async fn missing_generations_mars_or_changed_statement_never_borrow_the_first_holders_sql() {
    let focus = focus().await;
    for field in ["session_generation", "request_generation", "sql_handle", "request_id"] {
        let mut supplied = replies();
        let changed = match field {
            "sql_handle" => Val::Text("EF".repeat(64)),
            "request_id" => Val::Int(1),
            _ => Val::Text("2020-01-01T04:00:01".into()),
        };
        let owner = replace(&metadata(43), &[(field, changed)]);
        let mut rows = point(&task_row(), &[owner]);
        if field == "request_id" {
            rows[1] = replace(&rows[1], &[("holder_request_id", Val::Int(1))]);
        }
        supplied[13] = Ok(rows);
        let mut native = reader(supplied);
        let captured = native.capture_page_task_holders(&focus).await.unwrap();
        assert_eq!(captured.observation.before.holder.relation, PageTaskHolderRelation::QualifiedRequest);
        let after = captured.observation.after.holder.request.as_ref().unwrap();
        assert!(!after.statement.is_available(), "{field}");
        if field == "sql_handle" {
            assert_ne!(
                after.statement_identity,
                captured.observation.before.holder.request.as_ref().unwrap().statement_identity
            );
        } else {
            assert_ne!(after.key, captured.observation.before.holder.request.as_ref().unwrap().key, "{field}");
        }
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn native_sentinel_missing_owner_and_ambiguous_rows_never_choose_a_representative_or_read_owner_sql() {
    let focus = focus().await;
    for (kind, relation) in [
        ("sentinel", PageTaskHolderRelation::Sentinel),
        ("missing", PageTaskHolderRelation::Changed),
        ("ambiguous", PageTaskHolderRelation::Ambiguous),
        ("foreign", PageTaskHolderRelation::OtherDatabase),
    ] {
        let mut supplied = replies();
        let mut task = task_row();
        let owners = match kind {
            "sentinel" => {
                task = replace(&task, &[("task_blocking_session_id", Val::Int(-4))]);
                Vec::new()
            }
            "missing" => Vec::new(),
            "ambiguous" => vec![metadata(43), metadata(43)],
            _ => vec![replace(
                &metadata(43),
                &[
                    ("scope_basis", Val::Int(5)),
                    ("session_database", Val::Text("other".into())),
                    ("request_database_id", Val::Int(8)),
                ],
            )],
        };
        supplied[1] = Ok(point(&task, &owners));
        // No selected owner payload: final mapping, point, and context shift one slot earlier.
        supplied.remove(10).expect("unused holder SQL fixture is valid");
        supplied[12] = Ok(point(&task, &owners));
        let mut native = reader(supplied);
        let captured = native.capture_page_task_holders(&focus).await.unwrap();
        assert_eq!(captured.observation.before.holder.relation, relation, "{kind}");
        assert!(captured.observation.before.holder.request.is_none());
        assert_eq!(
            native.io.calls.iter().filter(|call| call.sql.contains("sys.dm_exec_sql_text(")).count(),
            1,
            "only the affected waiter SQL is read, {kind}"
        );
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn original_or_final_owner_context_change_stops_publication_without_another_reader_or_retry() {
    let focus = focus().await;
    for phase in [0, 14] {
        let mut supplied = replies();
        supplied[phase] = Ok(vec![replace(&context(), &[("updateability", Val::Text("READ_ONLY".into()))])]);
        let mut native = reader(supplied);
        let error = native.capture_page_task_holders(&focus).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert_eq!(native.query_count(), phase as u32 + 1);
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn terminal_budget_or_cancel_stops_have_no_followup_native_work_and_keep_close_ownership() {
    let focus = focus().await;
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::QueryLimit] {
        let mut supplied = replies();
        supplied[10] = Err(ReadError::Stopped(stop));
        supplied.truncate(11);
        let mut native = reader(supplied);
        let error = native.capture_page_task_holders(&focus).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(native.query_count(), 11);
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
        assert_eq!(native.io.closed, 1);
    }
    let mut native = super::super::tests::reader_with_native(FakeRead::new(replies()).with_query_limit(10));
    let error = native.capture_page_task_holders(&focus).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    assert_eq!(native.query_count(), 10);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unknown_original_scope_and_invalid_locator_are_zero_io_and_budget_clamps_never_widen_callers() {
    let mut focus = focus().await;
    focus.participant.partition = SourcePartition::Target;
    let mut native = reader(Vec::new());
    assert!(native.capture_page_task_holders(&focus).await.is_err());
    assert_eq!(native.query_count(), 0);
    let requested = CollectionBudget {
        max_queries: 2,
        statement_timeout_ms: 5,
        total_timeout_ms: 10,
        page_rows: 1,
        max_snapshot_rows: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_sql_bytes: 1024,
        ..CollectionBudget::default()
    };
    let bounded = bounded_budget(&requested);
    bounded.validate().unwrap();
    assert_eq!(bounded.max_queries, 2);
    assert_eq!(bounded.statement_timeout_ms, 5);
    assert_eq!(bounded.total_timeout_ms, 10);
    assert_eq!(bounded.page_bytes, 1024);
    assert_eq!(bounded.max_sql_bytes, 1024);
    assert_eq!(bounded_budget(&CollectionBudget::upper_limits()).max_queries, 28);
}

#[tokio::test]
async fn unavailable_final_index_name_preserves_raw_task_and_mapping_without_original_index_attribution() {
    let focus = focus().await;
    let mut supplied = replies();
    supplied[12] = Ok(vec![replace(&header(0, 4144), &[("index_name", Val::Null)])]);
    let mut native = reader(supplied);
    let captured = native.capture_page_task_holders(&focus).await.unwrap();
    assert_eq!(captured.observation.target_state, PageWriterTargetState::Unavailable);
    let page = captured.observation.page.as_ref().unwrap();
    assert!(page.task_wait.is_some());
    assert!(page.is_valid());
    assert_eq!(page.resolution, PageWaitResolution::ObservedAtBothBounds);
    assert_eq!(page.target, Some(focus.target.clone()), "the first mapping is an immutable native point");
    let final_target = &captured.observation.final_target.as_ref().unwrap().target;
    assert!(final_target.object.as_ref().unwrap().index_name.is_none());
    assert!(final_target.writer_target_key().is_err());
    assert_eq!(captured.observation.before.holder.relation, PageTaskHolderRelation::QualifiedRequest);
    assert!(focus.accepts_observation(&captured.context, &captured.observation));
    assert_eq!(native.query_count(), 15);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_task_seen_only_at_the_final_point_keeps_its_holder_without_a_disappearance_claim_or_new_mapping_work() {
    let focus = focus().await;
    let mut absent = point(&task_row(), &[]).remove(0);
    let null_fields = absent
        .columns()
        .filter(|(name, _)| {
            name.starts_with("checked_task_") || name.starts_with("task_") && *name != "task_visibility"
        })
        .map(|(name, _)| (name, Val::Null))
        .collect::<Vec<_>>();
    absent = replace(&absent, &null_fields);
    let mut native = reader(vec![
        Ok(vec![context()]),
        Ok(vec![absent]),
        Ok(vec![context()]),
        Ok(point(&task_row(), &[metadata(43)])),
        Ok(vec![context()]),
    ]);
    let captured = native.capture_page_task_holders(&focus).await.unwrap();
    assert!(captured.observation.page.is_none());
    assert!(captured.observation.final_target.is_none());
    assert_eq!(captured.observation.target_state, PageWriterTargetState::Unavailable);
    assert_eq!(captured.observation.before.holder.relation, PageTaskHolderRelation::Unavailable);
    assert_eq!(captured.observation.after.holder.relation, PageTaskHolderRelation::QualifiedRequest);
    assert_eq!(captured.observation.after.holder.request.as_ref().unwrap().key.session.id, "43");
    assert_eq!(native.query_count(), 5);
    assert!(
        native
            .io
            .calls
            .iter()
            .all(|call| { !call.sql.contains("sys.dm_db_page_info(") && !call.sql.contains("sys.dm_exec_sql_text(") })
    );
    assert!(focus.accepts_observation(&captured.context, &captured.observation));
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_resolving_task_pointer_cannot_admit_headers_or_sql_after_the_original_request_is_missing_or_recycled() {
    let focus = focus().await;
    for missing in [true, false] {
        let task = if missing {
            replace(
                &task_row(),
                &[
                    ("session_id", Val::Null),
                    ("session_generation", Val::Null),
                    ("request_id", Val::Null),
                    ("request_generation", Val::Null),
                    ("sql_handle", Val::Null),
                    ("statement_start", Val::Null),
                    ("statement_end", Val::Null),
                    ("request_database_id", Val::Null),
                    ("no_current_request", Val::Bool(true)),
                ],
            )
        } else {
            replace(&task_row(), &[("request_generation", Val::Text("2020-01-01T07:59:00".into()))])
        };
        // The native task slot can still resolve while its original client request is absent or replaced.
        // Neither that pointer nor another readable request grants permission to read the old target or SQL.
        let current = point(&task, &[metadata(43)]);
        let mut native = reader(vec![
            Ok(vec![context()]),
            Ok(current.clone()),
            Ok(vec![context()]),
            Ok(current),
            Ok(vec![context()]),
        ]);
        let captured = native.capture_page_task_holders(&focus).await.unwrap();
        let expected = if missing { PageWaitRequestState::Missing } else { PageWaitRequestState::Changed };
        assert_eq!(captured.observation.before.request, expected);
        assert_eq!(captured.observation.after.request, expected);
        assert_eq!(captured.observation.target_state, PageWriterTargetState::NoLongerObserved);
        assert!(captured.observation.page.is_none() && captured.observation.final_target.is_none());
        assert!(captured.observation.before.holder.request.is_none());
        assert!(captured.observation.after.holder.request.is_none());
        assert_eq!(native.query_count(), 5);
        assert!(
            native.io.calls.iter().all(|call| {
                !call.sql.contains("sys.dm_db_page_info(") && !call.sql.contains("sys.dm_exec_sql_text(")
            })
        );
        assert!(focus.accepts_observation(&captured.context, &captured.observation));
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
        assert_eq!(native.io.closed, 1);
    }
}

#[tokio::test]
async fn malformed_native_waiter_or_holder_pointers_stop_before_headers_payloads_or_a_second_point() {
    let focus = focus().await;
    for (field, malformed) in [
        ("task_blocking_address", "not-a-task-address"),
        ("holder_task_address", "0000000000000000"),
        ("holder_task_address", "00000000AAAA000Z"),
    ] {
        let mut current = point(&task_row(), &[metadata(43)]);
        let ordinal = usize::from(field == "holder_task_address");
        current[ordinal] = replace(&current[ordinal], &[(field, Val::Text(malformed.into()))]);
        let mut native = reader(vec![Ok(vec![context()]), Ok(current)]);
        let error = native.capture_page_task_holders(&focus).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage, "{field}={malformed}");
        assert_eq!(native.query_count(), 2);
        assert!(
            native.io.calls.iter().all(|call| {
                !call.sql.contains("sys.dm_db_page_info(") && !call.sql.contains("sys.dm_exec_sql_text(")
            })
        );
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
        assert_eq!(native.io.closed, 1);
    }
}

#[tokio::test]
async fn task_resource_diagnostics_do_not_replace_native_holder_fields_or_retire_the_same_page_wait() {
    let focus = focus().await;
    let before_resource = task_waits::test_buffer_resource(4144, "0000000B96434108");
    let duplicate_resource = task_waits::test_buffer_resource(4144, "0000000B96434109");
    let after_resource = task_waits::test_buffer_resource(4144, "0000000B96434110");
    let before = replace(&task_row(), &[("task_resource_description", Val::Text(before_resource.clone()))]);
    let after = replace(&task_row(), &[("task_resource_description", Val::Text(after_resource.clone()))]);
    let mut rows = point(&before, &[metadata(43)]);
    let duplicate = replace(
        &rows[0],
        &[
            ("task_resource_description", Val::Text(duplicate_resource.clone())),
            ("checked_task_resource", Val::Text(duplicate_resource)),
        ],
    );
    rows.insert(1, duplicate);
    let mut supplied = replies();
    supplied[1] = Ok(rows);
    supplied[13] = Ok(point(&after, &[metadata(43)]));
    let mut native = reader(supplied);
    let captured = native.capture_page_task_holders(&focus).await.unwrap();
    for point in [&captured.observation.before, &captured.observation.after] {
        assert_eq!(point.wait, PageWaitRelation::OriginalObserved);
        assert_eq!(point.holder.relation, PageTaskHolderRelation::QualifiedRequest);
        assert_eq!(point.holder.request.as_ref().unwrap().key.session.id, "43");
        assert_eq!(point.holder.task.as_ref().unwrap().task_address, "00000000AAAA0001");
    }
    assert_eq!(
        captured.observation.page.as_ref().unwrap().task_wait.as_ref().unwrap().resource.native.id,
        before_resource
    );
    assert_eq!(captured.observation.after_task.as_ref().unwrap().resource.native.id, after_resource);
    assert_eq!(focus.task.resource.native.id, "7:1:4144");
    assert!(focus.accepts_observation(&captured.context, &captured.observation));
    assert_eq!(native.query_count(), 15);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    assert_eq!(native.io.closed, 1);
}

#[tokio::test]
async fn duplicate_native_task_rows_cannot_hide_a_changed_page_wait_or_reported_holder() {
    let focus = focus().await;
    for (field, changed) in [
        ("task_resource_description", Val::Text(task_waits::test_buffer_resource(9999, "0000000B96434108"))),
        ("task_wait_type", Val::Text("PAGEIOLATCH_EX".into())),
        ("task_resource_address", Val::Text("00000000DDDD0002".into())),
        ("task_blocking_address", Val::Text("00000000BBBB0001".into())),
        ("task_wait_duration_ms", Val::Int(7)),
    ] {
        let mut current = point(&task_row(), &[metadata(43)]);
        let duplicate = replace(&current[0], &[(field, changed)]);
        current.insert(1, duplicate);
        let mut native = reader(vec![Ok(vec![context()]), Ok(current)]);
        let error = native.capture_page_task_holders(&focus).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(native.query_count(), 2);
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
        assert_eq!(native.io.closed, 1);
    }
}
