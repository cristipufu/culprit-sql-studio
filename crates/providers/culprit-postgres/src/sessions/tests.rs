use super::*;
use crate::rows::Row;
use std::collections::VecDeque;

const BACKEND: &str = "2026-11-15T08:00:00.000001Z";
const REQUEST: &str = "2026-11-15T09:00:00.000002Z";
const AT: &str = "2026-11-15T10:00:00.000003Z";

#[derive(Default)]
struct Reader {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: Vec<String>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> Result<Vec<Row>> {
        self.calls.push(sql.into());
        self.replies.pop_front().expect("unexpected native action read")
    }
}
fn row(fields: &[(&str, Option<&str>)]) -> Row {
    Row::from_fields(fields.iter().map(|(key, value)| ((*key).into(), value.map(str::to_owned))))
}
fn metadata() -> Row {
    row(&[
        ("build", Some("PostgreSQL 18.0")),
        ("database_oid", Some("16384")),
        ("database", Some("shop")),
        ("version", Some("180000")),
        ("block_bytes", Some("8192")),
        ("standby", Some("f")),
        ("started_at", Some("2026-11-14T00:00:00Z")),
        ("observed_at", Some(AT)),
        ("full_visibility", Some("t")),
        ("replay_pause_requested_access", Some("f")),
        ("replay_pause_state_access", Some("f")),
        ("pgss_schema", None),
        ("pgss_columns", None),
        ("pgss_info", Some("f")),
    ])
}
fn member(address: &str) -> Row {
    row(&[
        ("address", Some(address)),
        ("port", Some("5432")),
        ("system_identifier", Some("42")),
        ("observed_at", Some(AT)),
    ])
}
fn activity(backend: &str, request: &str, application: &str) -> Row {
    row(&[
        ("observed_at", Some(AT)),
        ("pid", Some("42")),
        ("backend_start", Some(backend)),
        ("query_start", Some(request)),
        ("database_oid", Some("16384")),
        ("database", Some("shop")),
        ("state", Some("active")),
        ("application_name", Some(application)),
        ("elapsed_ms", Some("2000")),
    ])
}
fn connector() -> Connector {
    Connector::new(&culprit_core::ConnectionSpec {
        host: "fixture.invalid".into(),
        database: Some("shop".into()),
        ..Default::default()
    })
    .unwrap()
}
async fn original() -> SessionActionTarget {
    let mut reader = Reader {
        replies: [
            Ok(vec![metadata()]),
            Ok(vec![member("127.0.0.1")]),
            Ok(vec![activity(BACKEND, REQUEST, "app")]),
            Ok(vec![metadata()]),
            Ok(vec![member("127.0.0.1")]),
        ]
        .into(),
        ..Default::default()
    };
    let connector = connector();
    let meta = native::metadata::activity(&mut reader, &connector.host, connector.port).await.unwrap();
    let before = Context::observe(&mut reader, &meta).await.unwrap().unwrap();
    let frame = native::activity::observe_rows(
        &mut reader,
        &native::activity::target_query(true, 42, "'client backend'", ""),
        &meta.database_oid,
        &CollectionBudget::default(),
    )
    .await
    .unwrap();
    let context = before.recheck(&mut reader, &connector.host, connector.port).await.unwrap().unwrap();
    super::target(&context, &frame.records[0]).unwrap()
}
fn attempt(point: Row, address: &str, signalled: bool) -> Reader {
    Reader {
        replies: [
            Ok(vec![metadata()]),
            Ok(vec![member("127.0.0.1")]),
            Ok(vec![point]),
            Ok(vec![metadata()]),
            Ok(vec![member(address)]),
            Ok(vec![row(&[("signalled", Some(if signalled { "t" } else { "f" }))])]),
        ]
        .into(),
        ..Default::default()
    }
}
async fn act(reader: &mut Reader, target: &SessionActionTarget, action: SessionAction) -> Result<SessionActionOutcome> {
    interrupt_from(reader, &connector(), target, action, &CollectionBudget::default()).await
}

#[tokio::test]
async fn original_native_generations_are_rechecked_and_success_only_acknowledges_the_signal() {
    let target = original().await;
    let mut reader = attempt(activity(BACKEND, REQUEST, "app"), "127.0.0.1", true);
    assert_eq!(act(&mut reader, &target, SessionAction::Cancel).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(reader.calls.len(), 6);
    let sql = reader.calls.last().unwrap();
    assert!(sql.contains("pg_cancel_backend(a.pid)"));
    assert!(sql.contains("a.backend_start=") && sql.contains("a.query_start="));
    assert!(sql.contains("backend_type='client backend'") && sql.contains("a.pid<>pg_catalog.pg_backend_pid()"));
    assert!(reader.calls[2].contains("WHERE pid=42"));
}

#[tokio::test]
async fn backend_request_member_and_role_changes_never_signal_the_new_target() {
    let target = original().await;
    for (backend, request, address) in [
        ("2026-11-15T08:00:01Z", REQUEST, "127.0.0.1"),
        (BACKEND, "2026-11-15T09:00:01Z", "127.0.0.1"),
        (BACKEND, REQUEST, "127.0.0.2"),
    ] {
        let mut reader = attempt(activity(backend, request, "app"), address, true);
        assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
        assert!(!reader.calls.iter().any(|sql| sql.contains("pg_cancel_backend")));
    }
    let mut target = target;
    target.context.database_writable = Some(false);
    let mut reader = attempt(activity(BACKEND, REQUEST, "app"), "127.0.0.1", true);
    assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
    assert_eq!(reader.calls.len(), 5);
}

#[tokio::test]
async fn termination_binds_the_session_even_after_its_request_changes() {
    let target = original().await;
    let mut reader = attempt(activity(BACKEND, "2026-11-15T09:00:01Z", "app"), "127.0.0.1", true);
    assert_eq!(act(&mut reader, &target, SessionAction::Terminate).await.unwrap(), SessionActionOutcome::SignalSent);
    let sql = reader.calls.last().unwrap();
    assert!(sql.contains("pg_terminate_backend") && !sql.contains("a.query_start="));
}

#[tokio::test]
async fn unknown_owner_self_and_native_false_are_explicit_failures() {
    let mut target = original().await;
    target.context.partition = SourcePartition::Target;
    let mut reader = Reader::default();
    assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
    assert!(reader.calls.is_empty());
    let target = original().await;
    let mut reader = attempt(activity(BACKEND, REQUEST, "culprit"), "127.0.0.1", true);
    assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
    assert_eq!(reader.calls.len(), 3);
    let mut reader = attempt(activity(BACKEND, REQUEST, "app"), "127.0.0.1", false);
    assert_eq!(
        act(&mut reader, &target, SessionAction::Cancel).await.unwrap_err().code,
        culprit_core::ErrorCode::QueryFailed
    );
}

#[test]
fn json_and_flat_native_timestamps_keep_the_same_microsecond_generation_without_rounding() {
    assert_eq!(native::activity::generation_timestamp("2026-11-15T10:00:00.000001+02:00").unwrap(), BACKEND);
    assert_eq!(native::activity::generation_timestamp(BACKEND).unwrap(), BACKEND);
    assert!(native::activity::generation_timestamp("2026-11-15T08:00:00.000001001Z").is_err());
    assert!(native::activity::generation_timestamp("not-a-native-time").is_err());
}

#[tokio::test]
async fn optional_identity_denial_withholds_actions_but_terminal_stops_still_propagate() {
    let connector = connector();
    let mut reader = Reader { replies: vec![Ok(vec![metadata()])].into(), ..Default::default() };
    let metadata = native::metadata::activity(&mut reader, &connector.host, connector.port).await.unwrap();
    for code in [culprit_core::ErrorCode::PermissionDenied, culprit_core::ErrorCode::Unsupported] {
        let mut reader = Reader { replies: vec![Err(Error::new(code, "fixture denied"))].into(), ..Default::default() };
        assert!(Context::observe(&mut reader, &metadata).await.unwrap().is_none());
        assert_eq!(reader.calls.len(), 1);
    }
    let mut reader = Reader { replies: vec![Err(CollectionStop::TimedOut.error())].into(), ..Default::default() };
    assert_eq!(
        CollectionStop::from_error(&Context::observe(&mut reader, &metadata).await.err().unwrap()),
        CollectionStop::TimedOut
    );
    assert_eq!(reader.calls.len(), 1);
}

#[tokio::test]
async fn source_offers_only_actions_supported_by_its_retained_native_rows() {
    let target = original().await;
    let record = native::activity::decode_row(activity(BACKEND, REQUEST, "app"), "16384").unwrap();
    let mut session = native::activity::project(&record);
    assert!(offered_actions(std::slice::from_ref(&session), culprit_core::Environment::Local).is_empty());
    session.action_target = Some(target);
    assert_eq!(
        offered_actions(std::slice::from_ref(&session), culprit_core::Environment::Local),
        vec![SessionAction::Cancel, SessionAction::Terminate]
    );
    assert!(offered_actions(std::slice::from_ref(&session), culprit_core::Environment::Production).is_empty());
    session.action_target.as_mut().unwrap().activity.request = None;
    assert_eq!(
        offered_actions(std::slice::from_ref(&session), culprit_core::Environment::Local),
        vec![SessionAction::Terminate]
    );
}

#[tokio::test]
async fn an_out_of_range_native_pid_is_malformed_input_before_any_native_read() {
    let mut target = original().await;
    for id in ["0", "2147483648", "4294967296", "99999999999"] {
        target.activity.session.id = id.into();
        let mut reader = Reader::default();
        let error = act(&mut reader, &target, SessionAction::Cancel).await.unwrap_err();
        assert_eq!(error.code, culprit_core::ErrorCode::InvalidInput);
        assert_eq!(error.message, format!("'{id}' is not a pid"));
        assert!(reader.calls.is_empty());
    }
}
