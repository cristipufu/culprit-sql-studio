use super::*;
use crate::{read::test_support::Read, rows::Rec};
use culprit_core::{Error, ErrorCode, SourceState};
use mysql_async::Value;
use test_support::*;

async fn collect(reader: &mut Read, focus: Option<&ActivityRequestFocus>) -> Result<Captured> {
    capture_from(reader, &connector(), 7, focus, &bounded_budget(&CollectionBudget::default()), PlanRead::None).await
}
fn after() -> Vec<Result<Vec<Rec>>> {
    bound(AFTER, "8.4.0", Some(UUID), false, 100, true)
}

#[tokio::test]
async fn one_held_native_profile_retains_exact_current_request_temp_and_parent_linked_untimed_stage() {
    let focus = original_focus();
    let mut io = reader(after(), Ok(vec![stage(&[])]), statement(&[]));
    let captured = collect(&mut io, Some(&focus)).await.unwrap();
    assert_eq!(io.query_count(), 23, "seven setup phases, both native bounds, selected point/stage and final actor");
    assert!(io.explains.is_empty());
    assert!(io.replies.is_empty());
    assert_eq!(captured.snapshot.result.binding, FocusedRequestBinding::Matched);
    assert_eq!(captured.snapshot.result.membership, FocusedRequestMembership::Observed);
    assert_eq!(captured.snapshot.coverage.state, SourceState::Collected);
    let [point] = captured.snapshot.observations.as_slice() else { panic!() };
    assert_eq!(point.actor, focus.original.key);
    assert_eq!(point.statement_identity, focus.statement_identity);
    assert_eq!(point.scope, ActivityScopeBasis::RequestDatabase);
    assert_eq!(point.native.mysql().unwrap().rows_examined.as_deref(), Some("9007199254740993"));
    assert_eq!(point.native.mysql().unwrap().created_tmp_tables.as_deref(), Some("1"));
    assert_eq!(point.native.mysql().unwrap().created_tmp_disk_tables.as_deref(), Some("0"));
    assert!(point.native.mysql().unwrap().sort_merge_passes.is_none());
    let MysqlStageObservation::Observed { event } = &point.native.mysql().unwrap().stage else { panic!() };
    assert_eq!(event.nesting_event_type, "STATEMENT");
    assert_eq!(event.nesting_event_id, point.native.mysql().unwrap().statement_event_id);
    assert!(event.timer_start_ps.is_none(), "an untimed stage can retain native parentage/work");
    assert_eq!(event.work_completed.as_deref(), Some("9007199254740993"));
    assert!(point.fits_source_scope(&description(&captured.snapshot)));
    let DefinitionPayload::Available { text, .. } = &point.statement else { panic!() };
    assert!(!text.contains("9264"));
}

#[tokio::test]
async fn participant_changes_never_install_the_current_row_or_a_plan_by_numeric_session() {
    for changes in
        [vec![("thread_id", 108u64.into())], vec![("event_id", 5u64.into())], vec![("timer_start", 6u64.into())]]
    {
        let focus = original_focus();
        let mut io = reader(after(), Ok(vec![stage(&[])]), statement(&changes));
        let captured = collect(&mut io, Some(&focus)).await.unwrap();
        assert_eq!(captured.snapshot.result.binding, FocusedRequestBinding::ParticipantChanged);
        assert_eq!(captured.snapshot.result.membership, FocusedRequestMembership::NotObserved);
        assert!(captured.snapshot.observations.is_empty());
        assert!(!captured.snapshot.coverage.complete_scope);
        assert_eq!(captured.snapshot.coverage.state, SourceState::Partial);
        assert!(captured.plan.plan.is_none());
    }
}

#[tokio::test]
async fn original_member_role_or_epoch_mismatch_stops_before_selected_native_work() {
    for (member, readonly, uptime) in
        [("f54a461c-0001-4000-8000-000000000002", false, 100), (UUID, true, 100), (UUID, false, 101)]
    {
        let mut io = Read::with(bound(BEFORE, "8.4.0", Some(member), readonly, uptime, true));
        let error = collect(&mut io, Some(&original_focus())).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert!(!io.queries.iter().any(|(sql, _, _)| sql.contains("t.PROCESSLIST_ID=?")
            || sql.contains("FROM performance_schema.events_stages_current")));
    }
    let mut io = Read::with(bound(BEFORE, "8.4.0", None, false, 100, true));
    assert_eq!(
        CollectionStop::from_error(&collect(&mut io, Some(&original_focus())).await.err().unwrap()),
        CollectionStop::SourceChanged
    );
    assert!(!io.queries.iter().any(|(sql, _, _)| sql.contains("t.PROCESSLIST_ID=?")));
}

#[tokio::test]
async fn native_owner_changes_at_completion_withhold_every_provisional_point() {
    for (member, readonly, uptime) in
        [("f54a461c-0001-4000-8000-000000000002", false, 100), (UUID, true, 100), (UUID, false, 101)]
    {
        let mut io =
            reader(bound(AFTER, "8.4.0", Some(member), readonly, uptime, true), Ok(vec![stage(&[])]), statement(&[]));
        let error = collect(&mut io, Some(&original_focus())).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert!(io.replies.is_empty());
    }
}

#[tokio::test]
async fn stage_denial_preserves_request_temp_counters_and_terminal_errors_issue_no_followup() {
    let mut io = reader(after(), Err(Error::new(ErrorCode::PermissionDenied, "native stage denied")), statement(&[]));
    let captured = collect(&mut io, Some(&original_focus())).await.unwrap();
    let point = &captured.snapshot.observations[0];
    assert_eq!(point.native.mysql().unwrap().created_tmp_disk_tables.as_deref(), Some("0"));
    assert_eq!(
        point.native.mysql().unwrap().stage,
        MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::PermissionDenied }
    );
    assert_eq!(captured.snapshot.coverage.state, SourceState::Partial);
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
    ] {
        let mut io = reader(after(), Err(stop.error()), statement(&[]));
        assert_eq!(CollectionStop::from_error(&collect(&mut io, Some(&original_focus())).await.err().unwrap()), stop);
        assert_eq!(
            io.queries.len(),
            6,
            "no actor recheck, context retry or process-list fallback after a terminal stage read"
        );
        assert!(io.explains.is_empty());
    }
}

#[tokio::test]
async fn known_current_owner_does_not_supply_missing_original_owner_and_session_database_stays_weaker() {
    let focus = original_focus_in(SourcePartition::Target);
    let mut io = reader(after(), Ok(vec![stage(&[])]), statement(&[]));
    let captured = collect(&mut io, Some(&focus)).await.unwrap();
    assert_eq!(captured.snapshot.result.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    let mut io = reader(after(), Ok(vec![stage(&[])]), statement(&[("request_db", Value::NULL)]));
    io.replies[4] = Ok(vec![statement(&[("request_db", Value::NULL)])]);
    let captured = collect(&mut io, None).await.unwrap();
    assert_eq!(captured.snapshot.observations[0].scope, ActivityScopeBasis::SessionDatabase);
    assert_eq!(captured.snapshot.coverage.state, SourceState::Partial);
    assert!(!captured.snapshot.coverage.complete_scope);
}

#[tokio::test]
async fn missing_and_foreign_scope_are_not_healthy_empty_or_a_completion_verdict() {
    for rows in [vec![], vec![statement(&[("request_db", "foreign".into())])]] {
        let mut replies = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
        replies.push(Ok(rows));
        replies.extend(after());
        let mut io = Read::with(replies);
        let captured = collect(&mut io, Some(&original_focus())).await.unwrap();
        assert!(captured.snapshot.observations.is_empty());
        assert!(!captured.snapshot.coverage.complete_scope);
        assert_eq!(captured.snapshot.coverage.state, SourceState::Partial);
    }
}

#[tokio::test]
async fn maria_stage_events_keep_parent_identity_but_never_project_mysql_only_work_columns() {
    let mut replies = bound(BEFORE, "11.8.9-MariaDB", Some(UID), false, 100, true);
    replies.push(Ok(vec![statement(&[])]));
    replies.push(Ok(vec![stage(&[("work_completed", Value::NULL), ("work_estimated", Value::NULL)])]));
    replies.push(Ok(vec![statement(&[])]));
    replies.extend(bound(AFTER, "11.8.9-MariaDB", Some(UID), false, 100, true));
    let mut io = Read::with(replies).with_native_phases().with_query_budget(MAX_SOURCE_QUERIES, 7);
    let captured = collect(&mut io, None).await.unwrap();
    assert!(captured.snapshot.is_valid());
    let sql =
        &io.queries.iter().find(|(sql, _, _)| sql.contains("FROM performance_schema.events_stages_current")).unwrap().0;
    assert!(sql.contains("NULL AS work_completed, NULL AS work_estimated"));
    assert!(!sql.contains("WORK_COMPLETED AS"));
    let MysqlStageObservation::Observed { event } = &captured.snapshot.observations[0].native.mysql().unwrap().stage
    else {
        panic!()
    };
    assert!(event.work_completed.is_none() && event.work_estimated.is_none());
    assert_eq!(event.nesting_event_type, "STATEMENT");
}

#[tokio::test]
async fn owned_live_projection_closes_on_success_error_and_unconfirmed_completion() {
    for shutdown in [SourceShutdown::Quiescent, SourceShutdown::Unconfirmed] {
        let mut io = reader(after(), Ok(vec![stage(&[])]), statement(&[]));
        io.shutdown = Some(shutdown);
        let result =
            query_progress_from(&mut io, &connector(), 7, &bounded_budget(&CollectionBudget::default()), false).await;
        assert_eq!(io.closed, 1);
        if shutdown == SourceShutdown::Unconfirmed {
            assert_eq!(CollectionStop::from_error(&result.err().unwrap()), CollectionStop::CancellationUnconfirmed);
        } else {
            let live = result.unwrap();
            assert_eq!(live.running, Some(true));
            assert!(live.current_work.is_some());
            assert!(live.phase_percent.is_some());
            assert!(live.percent.is_none());
        }
    }
    let mut io = reader(after(), Err(CollectionStop::TimedOut.error()), statement(&[]));
    assert!(
        query_progress_from(&mut io, &connector(), 7, &bounded_budget(&CollectionBudget::default()), false)
            .await
            .is_err()
    );
    assert_eq!(io.closed, 1);
}

#[tokio::test]
async fn retained_construction_is_lazy_and_profile_budget_never_widens_a_caller_dimension() {
    for focus in [original_focus(), original_focus_in(SourcePartition::Target)] {
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::CurrentRequestWork { focus: Box::new(focus) },
            budget: CollectionBudget::default(),
        };
        request.validate().unwrap();
        let mut source = source(connector(), &request).unwrap();
        assert_eq!(source.query_count(), 0);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(source.query_count(), 0);
    }
    let strict = CollectionBudget {
        max_queries: 1,
        max_snapshot_rows: 1,
        page_rows: 1,
        page_bytes: 1024,
        total_bytes: 1024,
        max_sql_bytes: 1024,
        max_plan_bytes: 1024,
        max_pages: 1,
        statement_timeout_ms: 1,
        total_timeout_ms: 1,
    };
    strict.validate().unwrap();
    assert_eq!(bounded_budget(&strict), strict);
    let mut io = reader(after(), Ok(vec![stage(&[])]), statement(&[])).with_query_budget(22, 7);
    assert_eq!(
        CollectionStop::from_error(&collect(&mut io, Some(&original_focus())).await.err().unwrap()),
        CollectionStop::QueryLimit
    );
}

#[tokio::test]
async fn retained_frozen_pages_reuse_the_completed_source_and_never_inspect_a_plan() {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::CurrentRequestWork { focus: Box::new(original_focus()) },
        budget: bounded_budget(&CollectionBudget::default()),
    };
    request.validate().unwrap();
    let io = reader(after(), Ok(vec![stage(&[])]), statement(&[]));
    let mut source = FrozenSource::new(Reader { io, connector: connector(), verified: None }, &request).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request));
    let count = source.query_count();
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_none());
    let TelemetryRecord::Operational(row) = &records[0] else { panic!() };
    let OperationalFact::CurrentRequestWork(point) = &row.fact else { panic!() };
    assert!(point.fits_source_scope(&description));
    assert!(matches!(source.finish().await.unwrap(), SourceCompletion::Verified));
    assert_eq!(source.query_count(), count);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn missing_current_member_keeps_raw_points_partial_without_borrowing_a_native_owner() {
    let mut replies = bound(BEFORE, "8.4.0", None, false, 100, true);
    replies.push(Ok(vec![statement(&[])]));
    replies.push(Ok(vec![stage(&[])]));
    replies.push(Ok(vec![statement(&[])]));
    replies.extend(bound(AFTER, "8.4.0", None, false, 100, true));
    let mut io = Read::with(replies);
    let captured = collect(&mut io, Some(&original_focus_in(SourcePartition::Target))).await.unwrap();
    assert_eq!(captured.snapshot.observations.len(), 1);
    assert_eq!(captured.snapshot.result.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    assert_eq!(captured.snapshot.coverage.state, SourceState::Partial);
    assert!(!captured.snapshot.context.has_qualified_local_owner());
}

#[tokio::test]
async fn disabled_stage_consumers_skip_that_read_and_changed_stage_configuration_keeps_statement_positives() {
    let mut replies = bound(BEFORE, "8.4.0", Some(UUID), false, 100, false);
    replies.push(Ok(vec![statement(&[])]));
    replies.push(Ok(vec![statement(&[])]));
    replies.extend(bound(AFTER, "8.4.0", Some(UUID), false, 100, false));
    let mut io = Read::with(replies);
    let captured = collect(&mut io, Some(&original_focus())).await.unwrap();
    assert_eq!(
        captured.snapshot.observations[0].native.mysql().unwrap().stage,
        MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::InstrumentationDisabled }
    );
    assert!(!io.queries.iter().any(|(sql, _, _)| sql.contains("FROM performance_schema.events_stages_current")));
    let mut io = reader(bound(AFTER, "8.4.0", Some(UUID), false, 100, false), Ok(vec![stage(&[])]), statement(&[]));
    let captured = collect(&mut io, Some(&original_focus())).await.unwrap();
    assert_eq!(captured.snapshot.observations[0].native.mysql().unwrap().created_tmp_tables.as_deref(), Some("1"));
    assert_eq!(captured.snapshot.coverage.state, SourceState::Partial);
    assert!(matches!(
        captured.snapshot.observations[0].native.mysql().unwrap().stage,
        MysqlStageObservation::Unavailable { .. }
    ));
}

#[tokio::test]
async fn an_absent_stage_consumer_is_unknown_at_either_bound_instead_of_explicitly_disabled() {
    let unknown = |at| {
        let mut rows = bound(at, "8.4.0", Some(UUID), false, 100, true);
        rows.last_mut().unwrap().as_mut().unwrap().pop();
        rows
    };
    let mut rows = unknown(BEFORE);
    rows.push(Ok(vec![statement(&[])]));
    rows.push(Ok(vec![statement(&[])]));
    rows.extend(unknown(AFTER));
    let mut io = Read::with(rows);
    let captured = collect(&mut io, Some(&original_focus())).await.unwrap();
    assert_eq!(
        captured.snapshot.observations[0].native.mysql().unwrap().stage,
        MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::NotCollected }
    );
    assert!(!io.queries.iter().any(|(sql, _, _)| sql.contains("FROM performance_schema.events_stages_current")));
    let mut io = reader(unknown(AFTER), Ok(vec![stage(&[])]), statement(&[]));
    let captured = collect(&mut io, Some(&original_focus())).await.unwrap();
    assert_eq!(
        captured.snapshot.observations[0].native.mysql().unwrap().stage,
        MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::NotCollected }
    );
    assert_eq!(captured.snapshot.observations[0].native.mysql().unwrap().created_tmp_tables.as_deref(), Some("1"));
    assert_eq!(captured.snapshot.coverage.state, SourceState::Partial);
}

#[tokio::test]
async fn mysql_native_live_plan_is_inside_the_same_event_bracket_and_has_twenty_five_owned_phases() {
    let json = r#"{"query_block":{"select_id":1,"table":{"table_name":"orders","access_type":"ALL","rows_examined_per_scan":1,"rows_produced_per_join":1,"filtered":"100"}}}"#;
    for changed in [false, true] {
        let mut replies = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
        replies.push(Ok(vec![statement(&[])]));
        replies.push(Ok(vec![stage(&[])]));
        replies.push(Ok(vec![Rec::from_values(vec![("EXPLAIN", json.into())])]));
        let changed_event = [("event_id", 5u64.into())];
        replies.push(Ok(vec![statement(if changed { &changed_event } else { &[] })]));
        replies.extend(after());
        let mut io = Read::with(replies).with_native_phases().with_query_budget(MAX_LIVE_QUERIES, 7);
        let live = query_progress_from(
            &mut io,
            &connector(),
            7,
            &bounded(&CollectionBudget::default(), MAX_LIVE_QUERIES),
            true,
        )
        .await
        .unwrap();
        assert_eq!(io.query_count(), 25);
        assert_eq!(io.closed, 1);
        assert_eq!(io.explains, ["EXPLAIN FORMAT=JSON FOR CONNECTION 7"]);
        assert_eq!(live.plan.is_some(), !changed);
        assert_eq!(live.running, (!changed).then_some(true));
        assert!(live.percent.is_none());
        if changed {
            assert!(live.current_work.as_ref().unwrap().observations.is_empty());
        } else {
            assert_eq!(live.plan.as_ref().unwrap().kind, culprit_core::PlanKind::Estimated);
        }
    }
}

#[tokio::test]
async fn mariadb_native_live_plan_keeps_operator_points_but_never_sums_stage_completion() {
    let json = r#"{"query_block":{"select_id":1,"r_loops":1,"r_total_time_ms":5,"table":{"table_name":"orders","access_type":"ALL","rows":100,"r_loops":1,"r_rows":10,"filtered":100,"r_filtered":100}}}"#;
    for (with_plan, changed) in [(false, false), (true, false), (true, true)] {
        let mut replies = bound(BEFORE, "11.8.9-MariaDB", Some(UID), false, 100, true);
        replies.push(Ok(vec![statement(&[])]));
        replies.push(Ok(vec![stage(&[("work_completed", Value::NULL), ("work_estimated", Value::NULL)])]));
        replies.push(Ok(vec![Rec::from_values(vec![("ANALYZE", json.into())])]));
        let changes = if changed { vec![("event_id", 5u64.into())] } else { vec![] };
        replies.push(Ok(vec![statement(&changes)]));
        replies.extend(bound(AFTER, "11.8.9-MariaDB", Some(UID), false, 100, true));
        let mut io = Read::with(replies).with_native_phases().with_query_budget(MAX_LIVE_QUERIES, 7);
        let live = query_progress_from(
            &mut io,
            &connector(),
            7,
            &bounded(&CollectionBudget::default(), MAX_LIVE_QUERIES),
            with_plan,
        )
        .await
        .unwrap();
        assert_eq!(io.query_count(), 25);
        assert_eq!(io.explains, ["SHOW ANALYZE FORMAT=JSON FOR 7"]);
        assert_eq!(live.running, (!changed).then_some(true));
        assert_eq!(live.plan.is_some(), with_plan && !changed);
        assert_eq!(live.nodes.is_empty(), changed);
        assert!(live.percent.is_none() && live.percent_basis.is_none() && live.phase_percent.is_none());
        assert_eq!(io.closed, 1);
    }
}

#[tokio::test]
async fn an_oversized_native_live_plan_stops_once_and_closes_without_followup_or_retry() {
    let mut replies = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
    replies.push(Ok(vec![statement(&[])]));
    replies.push(Ok(vec![stage(&[])]));
    replies.push(Ok(vec![Rec::from_values(vec![("EXPLAIN", "native plan exceeds caller limit".into())])]));
    let mut io = Read::with(replies).with_native_phases().with_query_budget(MAX_LIVE_QUERIES, 7);
    let budget = CollectionBudget { max_plan_bytes: 1, ..bounded(&CollectionBudget::default(), MAX_LIVE_QUERIES) };
    let error = query_progress_from(&mut io, &connector(), 7, &budget, true).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::ByteLimit);
    assert_eq!(io.closed, 1);
    assert_eq!(io.explains.len(), 1);
    assert_eq!(io.queries.len(), 6);
    assert!(io.replies.is_empty());
}
