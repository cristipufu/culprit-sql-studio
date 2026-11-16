use super::*;
use crate::{read::test_support::Read, rows::Rec};
use culprit_core::{Error, ErrorCode};
use mysql_async::Value;
use test_support::*;

async fn live(io: &mut Read) -> Result<QueryProgress> {
    query_progress_from(io, &connector(), 7, &bounded(&CollectionBudget::default(), MAX_LIVE_QUERIES), true).await
}
fn phase_reader(rows: Vec<Result<Vec<Rec>>>) -> Read {
    Read::with(rows).with_native_phases().with_query_budget(MAX_LIVE_QUERIES, 7)
}
fn process_reads(io: &Read) -> usize {
    io.queries.iter().filter(|(sql, _, _)| sql.contains("FROM information_schema.PROCESSLIST")).count()
}

#[tokio::test]
async fn performance_schema_off_keeps_one_independent_current_session_without_a_request_or_plan() {
    for version in ["8.4.0", "11.8.9-MariaDB"] {
        let mut rows = without_performance_schema(BEFORE, version);
        rows.push(Ok(vec![session_point(&[])]));
        rows.extend(without_performance_schema(AFTER, version));
        let mut io = phase_reader(rows);
        let progress = live(&mut io).await.unwrap();
        assert_eq!(progress.running, Some(true));
        assert!(progress.session_context.as_ref().unwrap().is_valid());
        assert!(!progress.session_context.as_ref().unwrap().context.has_qualified_local_owner());
        assert!(progress.current_work.is_none() && progress.request_key.is_none());
        assert!(progress.plan.is_none() && progress.nodes.is_empty() && progress.percent.is_none());
        assert!(progress.elapsed_ms.is_none());
        assert!(progress.statement.is_some());
        assert_eq!(io.query_count(), 13);
        assert_eq!(io.closed, 1);
        assert_eq!(process_reads(&io), 1);
        assert!(io.explains.is_empty());
    }
}

#[tokio::test]
async fn disabled_statement_consumers_do_not_hide_a_processlist_point_or_enable_native_instrumentation() {
    let mut rows = without_statement_consumers(BEFORE);
    rows.push(Ok(vec![session_point(&[])]));
    rows.extend(without_statement_consumers(AFTER));
    let mut io = phase_reader(rows);
    let progress = live(&mut io).await.unwrap();
    assert_eq!(progress.running, Some(true));
    assert!(progress.current_work.is_none());
    assert_eq!(io.query_count(), 19);
    assert_eq!(process_reads(&io), 1);
    assert!(io.explains.is_empty());
    assert!(!io.queries.iter().any(
        |(sql, _, _)| sql.contains("LEFT JOIN performance_schema.events_statements_current")
            || sql.contains("FROM performance_schema.events_stages_current")
            || sql.starts_with("UPDATE")
    ));
}

#[tokio::test]
async fn safely_denied_optional_current_statement_can_read_one_independent_point_in_twenty_one_phases() {
    for code in [ErrorCode::PermissionDenied, ErrorCode::Unsupported] {
        let mut rows = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
        rows.push(Err(Error::new(code, "optional statement visibility denied")));
        rows.push(Ok(vec![session_point(&[])]));
        rows.extend(bound(AFTER, "8.4.0", Some(UUID), false, 100, true));
        let mut io = phase_reader(rows);
        let progress = live(&mut io).await.unwrap();
        assert_eq!(progress.running, Some(true));
        assert!(progress.current_work.is_none());
        assert!(progress.session_context.as_ref().unwrap().context.has_qualified_local_owner());
        assert_eq!(io.query_count(), 21);
        assert_eq!(io.closed, 1);
        assert_eq!(process_reads(&io), 1);
        assert!(io.explains.is_empty());
    }
}

#[tokio::test]
async fn hidden_optional_context_uses_the_same_metadata_acquisition_without_weakening_retained_admission() {
    let denied = || Err(Error::new(ErrorCode::PermissionDenied, "consumer visibility denied"));
    let context = |at| {
        let mut rows = bound(at, "8.4.0", Some(UUID), false, 100, true);
        *rows.last_mut().unwrap() = denied();
        rows
    };
    let mut rows = context(BEFORE);
    rows.push(Ok(vec![session_point(&[])]));
    rows.extend(context(AFTER));
    let mut io = phase_reader(rows);
    let progress = live(&mut io).await.unwrap();
    assert_eq!(progress.running, Some(true));
    assert_eq!(io.query_count(), 19);
    assert_eq!(process_reads(&io), 1);
    let mut io = phase_reader(context(BEFORE));
    let error = capture_from(
        &mut io,
        &connector(),
        7,
        Some(&original_focus()),
        &bounded_budget(&CollectionBudget::default()),
        PlanRead::None,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(process_reads(&io), 0);
}

#[tokio::test]
async fn absent_and_hidden_processlist_rows_are_unknown_while_only_observed_sleep_is_idle() {
    for (point, expected) in [
        (vec![], None),
        (vec![session_point(&[("db", "other".into())])], None),
        (
            vec![session_point(&[
                ("command", "Sleep".into()),
                ("sql_text", Value::NULL),
                ("sql_text_bytes", Value::NULL),
            ])],
            Some(false),
        ),
    ] {
        let mut rows = without_performance_schema(BEFORE, "8.4.0");
        rows.push(Ok(point));
        rows.extend(without_performance_schema(AFTER, "8.4.0"));
        let mut io = phase_reader(rows);
        let progress = live(&mut io).await.unwrap();
        assert_eq!(progress.running, expected);
        assert!(progress.current_work.is_none() && progress.plan.is_none());
        assert_eq!(process_reads(&io), 1);
        assert_eq!(io.closed, 1);
    }
}

#[tokio::test]
async fn an_observed_native_idle_row_and_a_changed_event_are_not_the_same_availability_result() {
    let mut rows = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
    rows.push(Ok(vec![statement(&[("command", "Sleep".into())])]));
    rows.extend(bound(AFTER, "8.4.0", Some(UUID), false, 100, true));
    let mut io = phase_reader(rows);
    assert_eq!(live(&mut io).await.unwrap().running, Some(false));
    assert_eq!(process_reads(&io), 0);
    let mut io = reader(
        bound(AFTER, "8.4.0", Some(UUID), false, 100, true),
        Ok(vec![stage(&[])]),
        statement(&[("event_id", 5u64.into())]),
    );
    let progress = query_progress_from(&mut io, &connector(), 7, &bounded_budget(&CollectionBudget::default()), false)
        .await
        .unwrap();
    assert_eq!(progress.running, None);
    assert!(progress.session_context.is_none() && progress.request_key.is_none());
    assert!(progress.plan.is_none() && progress.statement.is_none());
    assert_eq!(process_reads(&io), 0);
}

#[tokio::test]
async fn explicit_foreign_native_request_never_falls_back_to_its_session_default_database() {
    let mut rows = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
    rows.push(Ok(vec![statement(&[("request_db", "other".into())])]));
    rows.extend(bound(AFTER, "8.4.0", Some(UUID), false, 100, true));
    let mut io = phase_reader(rows);
    let progress = live(&mut io).await.unwrap();
    assert_eq!(progress.running, None);
    assert!(progress.session_context.is_none());
    assert_eq!(process_reads(&io), 0);
    assert!(io.explains.is_empty());
}

#[tokio::test]
async fn terminal_or_malformed_native_work_never_dispatches_a_processlist_fallback() {
    for error in [
        CollectionStop::Cancelled.error(),
        CollectionStop::TimedOut.error(),
        CollectionStop::CancellationUnconfirmed.error(),
        CollectionStop::QueryLimit.error(),
        CollectionStop::ByteLimit.error(),
        CollectionStop::InvalidPage.error(),
        Error::new(ErrorCode::QueryFailed, "unknown native failure"),
    ] {
        let mut rows = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
        let code = error.code;
        rows.push(Err(error));
        let mut io = phase_reader(rows);
        assert_eq!(live(&mut io).await.err().unwrap().code, code);
        assert_eq!(process_reads(&io), 0);
        assert_eq!(io.closed, 1);
        assert!(io.explains.is_empty());
    }
}

#[tokio::test]
async fn fallback_keeps_source_changes_close_uncertainty_and_the_original_total_budget_terminal() {
    for change in [("connection_id", 91u64.into()), ("read_only", 1u64.into()), ("server_id", 2u64.into())] {
        let mut rows = without_performance_schema(BEFORE, "8.4.0");
        rows.push(Ok(vec![session_point(&[])]));
        let mut after = without_performance_schema(AFTER, "8.4.0");
        let row = after.remove(0).unwrap().remove(0);
        rows.push(Ok(vec![row.with_fields(&[change])]));
        let mut io = phase_reader(rows);
        assert_eq!(CollectionStop::from_error(&live(&mut io).await.err().unwrap()), CollectionStop::SourceChanged);
        assert_eq!(io.closed, 1);
        assert_eq!(process_reads(&io), 1);
    }
    let mut rows = without_performance_schema(BEFORE, "8.4.0");
    rows.push(Ok(vec![session_point(&[])]));
    rows.extend(without_performance_schema(AFTER, "8.4.0"));
    let mut io = phase_reader(rows);
    io.shutdown = Some(SourceShutdown::Unconfirmed);
    assert_eq!(
        CollectionStop::from_error(&live(&mut io).await.err().unwrap()),
        CollectionStop::CancellationUnconfirmed
    );
    assert_eq!(io.closed, 1);
    let mut rows = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
    rows.push(Err(Error::new(ErrorCode::PermissionDenied, "optional statement visibility denied")));
    rows.push(Ok(vec![session_point(&[])]));
    rows.extend(bound(AFTER, "8.4.0", Some(UUID), false, 100, true));
    let mut io = phase_reader(rows).with_query_budget(20, 7);
    assert_eq!(CollectionStop::from_error(&live(&mut io).await.err().unwrap()), CollectionStop::QueryLimit);
    assert_eq!(io.query_count(), 20);
    assert_eq!(process_reads(&io), 1);
    assert_eq!(io.closed, 1);
}
