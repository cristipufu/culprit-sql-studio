use super::*;
use crate::{progress::test_support::*, read::test_support::Read};

async fn context() -> Context {
    Context::capture_live(&mut Read::with(without_performance_schema(BEFORE, "8.4.0")), &connector()).await.unwrap()
}
fn bounds(context: &Context) -> NativeReadContext {
    NativeReadContext { observed_until: canonical_time(AFTER).unwrap(), ..context.current_context().unwrap() }
}

#[tokio::test]
async fn one_bounded_default_database_session_point_retains_no_native_request_or_plan_authority() {
    let context = context().await;
    let mut io = Read::with(vec![Ok(vec![session_point(&[])])]);
    let capture = read(&mut io, 7, &context, 16 * 1024).await.unwrap();
    let native_context = bounds(&context);
    capture.validate(&native_context).unwrap();
    let progress = capture.project("7", native_context);
    assert_eq!(progress.running, Some(true));
    assert!(progress.session_context.is_some());
    assert!(progress.current_work.is_none());
    assert!(
        progress.plan.is_none()
            && progress.nodes.is_empty()
            && progress.percent.is_none()
            && progress.elapsed_ms.is_none()
    );
    assert_eq!(progress.statement.as_deref(), Some("ALTER TABLE orders ADD INDEX ix_customer(customer_id)"));
    let (sql, parameters, limit) = &io.queries[0];
    assert!(sql.contains("WHERE ID=? AND DB=?"));
    assert!(sql.contains("OCTET_LENGTH(INFO)<=?"));
    assert!(!sql.contains("TIME AS") && !sql.contains("TIME_MS") && !sql.contains("performance_schema"));
    assert_eq!(parameters.len(), 3);
    assert_eq!(*limit, 2);
    assert!(io.explains.is_empty());
}

#[tokio::test]
async fn absent_foreign_and_unsupported_command_points_do_not_become_completed_requests() {
    let context = context().await;
    for rows in [
        vec![],
        vec![session_point(&[("db", "foreign".into())])],
        vec![session_point(&[("command", "Prepare".into())])],
    ] {
        let mut io = Read::with(vec![Ok(rows)]);
        let progress = read(&mut io, 7, &context, 16 * 1024).await.unwrap().project("7", bounds(&context));
        assert_eq!(progress.running, None);
        assert!(progress.current_work.is_none() && progress.plan.is_none());
    }
    let mut io = Read::with(vec![Ok(vec![session_point(&[
        ("command", "Sleep".into()),
        ("sql_text", Value::NULL),
        ("sql_text_bytes", Value::NULL),
    ])])]);
    assert_eq!(
        read(&mut io, 7, &context, 16 * 1024).await.unwrap().project("7", bounds(&context)).running,
        Some(false)
    );
}

#[tokio::test]
async fn maria_native_stage_zero_and_independent_units_never_become_whole_statement_completion() {
    let mut context = context().await;
    context.connection.flavor = Flavor::Mariadb;
    for percent in [0.0, 37.5, 100.0] {
        let mut io = Read::with(vec![Ok(vec![session_point(&[
            ("stage", 2u64.into()),
            ("max_stage", 3u64.into()),
            ("progress", Value::Double(percent)),
        ])])]);
        let progress = read(&mut io, 7, &context, 16 * 1024).await.unwrap().project("7", bounds(&context));
        assert_eq!(progress.phase_percent, Some(percent / 100.0));
        assert_eq!(progress.phase.as_deref(), Some("copy to tmp table (stage 2 of 3)"));
        assert!(progress.percent.is_none() && progress.percent_basis.is_none() && progress.elapsed_ms.is_none());
        assert!(io.queries[0].0.contains("PROGRESS AS progress"));
    }
}

#[tokio::test]
async fn malformed_duplicate_and_own_session_points_are_terminal_without_an_alternative_read() {
    let context = context().await;
    for rows in [
        vec![session_point(&[("id", 8u64.into())])],
        vec![session_point(&[("progress", Value::Double(f64::NAN))])],
        vec![session_point(&[]), session_point(&[])],
    ] {
        let mut io = Read::with(vec![Ok(rows)]);
        assert_eq!(
            CollectionStop::from_error(&read(&mut io, 7, &context, 16 * 1024).await.err().unwrap()),
            CollectionStop::InvalidPage
        );
        assert_eq!(io.queries.len(), 1);
        assert!(io.explains.is_empty());
    }
    let mut io = Read::with(vec![Ok(vec![session_point(&[("observed_at", "2021-01-01T00:00:00Z".into())])])]);
    let capture = read(&mut io, 7, &context, 16 * 1024).await.unwrap();
    assert!(capture.validate(&context.current_context().unwrap()).is_err());
    let mut context = context;
    context.connection.connection_id = 7;
    let mut io = Read::with(vec![Ok(vec![session_point(&[])])]);
    assert!(read(&mut io, 7, &context, 16 * 1024).await.is_err());
    assert_eq!(io.queries.len(), 1);
}
