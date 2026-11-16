use super::*;
use culprit_postgres::wire::Row;
use serde_json::json;
use std::collections::VecDeque;

const NODE: &str = "3d2d9e10555b4ae2a683a8402505e416";
const OTHER_NODE: &str = "3d2d9e10555b4ae2a683a8402505e417";
const BACKEND: &str = "2020-01-01T05:30:00.000001Z";
const REQUEST: &str = "2020-01-01T05:59:00.000002Z";
const NOW: &str = "2020-01-01T06:00:00.000003Z";

#[derive(Default)]
struct Reader {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: Vec<String>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> Result<Vec<Row>> {
        self.calls.push(sql.into());
        self.replies.pop_front().expect("no native work outside the observed action")
    }
}
fn row(fields: &[(&str, Option<&str>)]) -> Row {
    Row::from_fields(fields.iter().map(|(key, value)| ((*key).into(), value.map(str::to_owned))))
}
fn context(node: &str, started: &str) -> Vec<Result<Vec<Row>>> {
    vec![
        Ok(vec![row(&[
            ("version", Some("PostgreSQL 15.2-YB-2025.2.0.0-b0")),
            ("compatibility", Some("150002")),
            ("database", Some("shop")),
            ("oid", Some("42")),
            ("now", Some(NOW)),
            ("started", Some(started)),
            ("global_views", Some("f")),
            ("statement_schema", None),
            ("statement_info", Some("f")),
            ("cluster_statements", Some("f")),
            ("metrics", Some("f")),
            ("rpc_metrics", Some("f")),
            ("role_oid", Some("10")),
            ("all_stats", Some("t")),
        ])]),
        Ok(vec![row(&[("node", Some(node))])]),
        Ok(vec![row(&[("backend", Some("777"))])]),
    ]
}
fn point(backend: &str, request: &str, application: &str) -> Row {
    row(&[
        ("observed_at", Some(NOW)),
        ("pid", Some("42")),
        ("backend_start", Some(backend)),
        ("query_start", Some(request)),
        ("database_oid", Some("42")),
        ("database", Some("shop")),
        ("state", Some("active")),
        ("application_name", Some(application)),
    ])
}
fn raw(node: &str, pid: u32, backend: &str) -> Value {
    json!({"server_uuid":node,"pid":pid,"datid":42,"datname":"shop","backend_type":"client backend",
        "backend_start":backend,"query_start":REQUEST,"state":"active","application_name":"app"})
}
async fn original_target() -> SessionActionTarget {
    let mut replies = context(NODE, "2020-01-01T05:00:00Z");
    replies.extend(context(NODE, "2020-01-01T05:00:00Z"));
    let mut reader = Reader { replies: replies.into(), ..Default::default() };
    let before = native::metadata::read(&mut reader, "fixture.invalid", 5433).await.unwrap();
    let after = native::metadata::read(&mut reader, "fixture.invalid", 5433).await.unwrap();
    let context = before.completed_action_context(&after).unwrap();
    let row = raw(NODE, 42, BACKEND);
    let observed = original(&row, &before.probe, NODE).unwrap();
    target(&row, &observed, Some(&context), NODE).unwrap()
}
fn attempt(point: Row, after_node: &str, after_start: &str, signalled: bool) -> Reader {
    let mut replies = context(NODE, "2020-01-01T05:00:00Z");
    replies.extend(context(after_node, after_start));
    replies.push(Ok(vec![point]));
    replies.push(Ok(vec![row(&[("signalled", Some(if signalled { "t" } else { "f" }))])]));
    Reader { replies: replies.into(), ..Default::default() }
}
async fn act(reader: &mut Reader, target: &SessionActionTarget, action: SessionAction) -> Result<SessionActionOutcome> {
    interrupt_from(reader, "fixture.invalid", 5433, target, action, &CollectionBudget::default()).await
}

#[tokio::test]
async fn canonical_native_key_and_request_generation_reach_only_the_local_guarded_signal() {
    let target = original_target().await;
    assert_eq!(target.context.database_writable, None, "YSQL recovery state is not database updateability proof");
    assert!(target.is_valid_for(SessionAction::Cancel));
    assert_eq!(target.activity.session.id, format!("{NODE}/42"));
    assert_eq!(target.activity.session.generation, BACKEND);
    assert_eq!(target.activity.request.as_ref().unwrap().generation, REQUEST);
    assert_eq!(
        culprit_core::Engine::Yugabyte.spec().live_session_id(&target.activity.session),
        format!("{NODE}/42/{BACKEND}")
    );
    let mut reader = attempt(point(BACKEND, REQUEST, "app"), NODE, "2020-01-01T05:00:00Z", true);
    assert_eq!(act(&mut reader, &target, SessionAction::Cancel).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(reader.calls.len(), ACTION_QUERIES as usize - 1, "the owner adds its one bounded setup phase");
    let sql = reader.calls.last().unwrap();
    assert!(sql.contains("pg_cancel_backend(a.pid)"));
    assert!(
        sql.contains("a.backend_start=")
            && sql.contains("a.query_start=")
            && sql.contains("yb_get_local_tserver_uuid()")
    );
    assert!(sql.contains("backend_type='client backend'") && sql.contains("a.pid<>pg_catalog.pg_backend_pid()"));
    assert!(!sql.contains("gv$"));
}

#[tokio::test]
async fn different_backend_request_node_or_process_cannot_inherit_the_observed_target() {
    let target = original_target().await;
    for (backend, request, node, started) in [
        ("2020-01-01T05:30:01Z", REQUEST, NODE, "2020-01-01T05:00:00Z"),
        (BACKEND, "2020-01-01T05:59:01Z", NODE, "2020-01-01T05:00:00Z"),
        (BACKEND, REQUEST, OTHER_NODE, "2020-01-01T05:00:00Z"),
        (BACKEND, REQUEST, NODE, "2020-01-01T05:00:01Z"),
    ] {
        let mut reader = attempt(point(backend, request, "app"), node, started, true);
        assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
        assert!(!reader.calls.iter().any(|sql| sql.contains("pg_cancel_backend")));
    }
}

#[tokio::test]
async fn termination_remains_session_scoped_and_native_false_is_failure() {
    let target = original_target().await;
    let mut reader = attempt(point(BACKEND, "2020-01-01T05:59:01Z", "app"), NODE, "2020-01-01T05:00:00Z", true);
    assert_eq!(act(&mut reader, &target, SessionAction::Terminate).await.unwrap(), SessionActionOutcome::SignalSent);
    assert!(!reader.calls.last().unwrap().contains("a.query_start="));
    let mut reader = attempt(point(BACKEND, REQUEST, "app"), NODE, "2020-01-01T05:00:00Z", false);
    assert_eq!(
        act(&mut reader, &target, SessionAction::Cancel).await.unwrap_err().code,
        culprit_core::ErrorCode::QueryFailed
    );
}

#[tokio::test]
async fn remote_and_pooled_rows_keep_native_identity_without_receiving_the_local_action_context() {
    let target = original_target().await;
    let mut reader = Reader { replies: context(NODE, "2020-01-01T05:00:00Z").into(), ..Default::default() };
    let metadata = native::metadata::read(&mut reader, "fixture.invalid", 5433).await.unwrap();
    let remote = raw(OTHER_NODE, 42, BACKEND);
    let observed = original(&remote, &metadata.probe, OTHER_NODE).unwrap();
    assert_ne!(observed.key.session, target.activity.session);
    assert!(super::target(&remote, &observed, Some(&target.context), OTHER_NODE).is_none());
    let mut pooled = raw(NODE, 42, BACKEND);
    pooled["backend_type"] = "yb-conn-mgr worker connection".into();
    let observed = original(&pooled, &metadata.probe, NODE).unwrap();
    assert!(super::target(&pooled, &observed, Some(&target.context), NODE).is_none());
    pooled["backend_start"] = Value::Null;
    assert!(original(&pooled, &metadata.probe, NODE).is_none());
}

#[tokio::test]
async fn source_cancellation_and_unqualified_targets_stop_without_a_signal_or_reopen() {
    let mut target = original_target().await;
    target.context.partition = SourcePartition::Target;
    let mut reader = Reader::default();
    assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
    assert!(reader.calls.is_empty());
    let target = original_target().await;
    let mut reader = Reader { replies: vec![Err(CollectionStop::Cancelled.error())].into(), ..Default::default() };
    assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
    assert_eq!(reader.calls.len(), 1);
}

#[tokio::test]
async fn unknown_native_role_is_not_rewritten_to_match_a_claimed_known_role() {
    let mut target = original_target().await;
    target.context.database_writable = Some(true);
    let mut reader = attempt(point(BACKEND, REQUEST, "app"), NODE, "2020-01-01T05:00:00Z", true);
    assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
    assert!(!reader.calls.iter().any(|sql| sql.contains("pg_cancel_backend")));
}

#[tokio::test]
async fn the_source_offers_actions_only_for_qualified_rows_and_retains_the_production_refusal() {
    let target = original_target().await;
    let mut session = culprit_core::Session {
        id: culprit_core::Engine::Yugabyte.spec().live_session_id(&target.activity.session),
        action_target: Some(target),
        user: None,
        application: None,
        host: None,
        database: None,
        state: "active".into(),
        command: None,
        wait: None,
        wait_ms: None,
        wait_resources: None,
        blocked_by: None,
        elapsed_ms: None,
        transaction_ms: None,
        cpu_ms: None,
        reads: None,
        sql: None,
        waiting_on_lock: None,
    };
    assert_eq!(
        offered(std::slice::from_ref(&session), culprit_core::Environment::Local),
        vec![SessionAction::Cancel, SessionAction::Terminate]
    );
    assert!(offered(std::slice::from_ref(&session), culprit_core::Environment::Production).is_empty());
    session.action_target.as_mut().unwrap().activity.request = None;
    assert_eq!(
        offered(std::slice::from_ref(&session), culprit_core::Environment::Local),
        vec![SessionAction::Terminate]
    );
    session.action_target = None;
    assert!(offered(std::slice::from_ref(&session), culprit_core::Environment::Local).is_empty());
}
