use super::*;
use crate::connect::{COLLECTOR, INTERACTIVE};
use culprit_core::{ConnectionSpec, Engine};

fn connector() -> Connector {
    Connector::new(&ConnectionSpec {
        engine: Some(Engine::Postgres),
        host: "no-native-dispatch.invalid".into(),
        port: Some(9),
        database: Some("unconnected".into()),
        user: Some("monitor".into()),
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn normal_setup_preserves_the_original_native_batch_and_independent_collector_setup() {
    for limits in [COLLECTOR, INTERACTIVE] {
        let scope = ApplicationReadScope::new();
        let reader = ReadSession::application(connector(), limits, scope.clone());
        assert_eq!(reader.setup_sql(), crate::connect::session_setup(limits));
        assert_eq!(reader.deadline, scope.deadline);
        assert!(reader.remaining().unwrap() <= limits.statement_timeout);
        assert_eq!(reader.budget.page_bytes as usize, culprit_core::limits::MAX_RESULT_BYTES);
        assert_eq!(reader.queries(), 0);
        assert!(reader.metrics().unwrap().templates.is_empty());
        assert!(reader.client.is_none());
    }
    let collector = ReadSession::new(connector(), CollectionBudget::default());
    let mut original = crate::connect::session_setup(SessionLimits {
        statement_timeout: Duration::from_millis(CollectionBudget::default().statement_timeout_ms.into()),
        catalog_path: true,
    });
    original.push_str("; SET max_parallel_workers_per_gather = 0 /* culprit */; SET work_mem = '4MB' /* culprit */; SET extra_float_digits = 3 /* culprit */");
    assert_eq!(collector.setup_sql(), original);
    assert!(collector.application_scope.is_none());
}

#[test]
fn parallel_normal_readers_share_one_exact_query_allowance_without_renewal() {
    let scope = ApplicationReadScope::new();
    let peer = scope.clone();
    for ordinal in 0..APPLICATION_QUERY_LIMIT {
        if ordinal % 2 == 0 {
            scope.start().unwrap();
        } else {
            peer.start().unwrap();
        }
    }
    assert_eq!(scope.state.lock().unwrap().queries, APPLICATION_QUERY_LIMIT);
    assert_eq!(CollectionStop::from_error(&peer.start().unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(CollectionStop::from_error(&scope.finish().unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(scope.deadline, peer.deadline);
    assert_eq!(scope.state.lock().unwrap().queries, APPLICATION_QUERY_LIMIT);
}

#[tokio::test]
async fn an_expired_normal_scope_dispatches_no_setup_or_source_sql() {
    let connection = connector();
    let scope = ApplicationReadScope::until(Instant::now() - Duration::from_nanos(1));
    let mut reader = ReadSession::application(connection.clone(), COLLECTOR, scope);
    let error = reader.query("SELECT /* culprit */ 1", 1).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert_eq!(reader.queries(), 0);
    assert!(reader.metrics().unwrap().templates.is_empty());
    assert!(reader.client.is_none());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(connection.source_shutdown(), SourceShutdown::Quiescent);
}

#[tokio::test]
async fn an_exhausted_shared_allowance_cannot_be_borrowed_for_another_session_setup() {
    let scope = ApplicationReadScope::new();
    for _ in 0..APPLICATION_QUERY_LIMIT {
        scope.start().unwrap();
    }
    let mut reader = ReadSession::application(connector(), COLLECTOR, scope.clone());
    let error = reader.query("SELECT /* culprit */ 1", 1).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    assert_eq!(reader.queries(), 0);
    assert!(reader.metrics().unwrap().templates.is_empty());
    assert!(reader.client.is_none());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(scope.state.lock().unwrap().queries, APPLICATION_QUERY_LIMIT);
}

#[test]
fn optional_existing_readers_keep_their_tighter_budget_deadline_and_original_setup() {
    let budget = CollectionBudget { max_queries: 5, total_timeout_ms: 500, ..Default::default() };
    let original = ReadSession::new(connector(), budget.clone());
    let deadline = original.deadline;
    let setup = original.setup_sql();
    let scoped = original.under_scope(ApplicationReadScope::new());
    assert_eq!(scoped.deadline, deadline);
    assert_eq!(scoped.budget, budget);
    assert_eq!(scoped.setup_sql(), setup);
    let parent = ApplicationReadScope::until(Instant::now() + Duration::from_millis(1));
    let expected = parent.deadline;
    let scoped = ReadSession::new(connector(), CollectionBudget::default()).under_scope(parent);
    assert_eq!(scoped.deadline, expected);
}

#[tokio::test]
async fn a_native_fence_blocks_every_normal_sibling_without_spending_another_query_slot() {
    let connection = connector();
    let scope = ApplicationReadScope::new();
    let mut reader = ReadSession::application(connection.clone(), COLLECTOR, scope.clone());
    connection.fence.mark_uncertain();
    let error = reader.query("SELECT /* culprit */ 1", 1).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(reader.queries(), 0);
    assert_eq!(scope.state.lock().unwrap().queries, 0);
    assert!(reader.metrics().unwrap().templates.is_empty());
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
}
