use super::*;
use crate::{
    native::activity::test_support::{consumers, row, row_with},
    read::{OwnedRead, test_support::Read},
};

const UUID: &str = "f54a461c-0001-4000-8000-000000000001";
const MARIADB_UID: &str = "q83hHHfky00d9QAi5bwmy+JVlfE=";
const AT: &str = "2020-01-01T06:00:00Z";
const LATER: &str = "2020-01-01T06:00:00.200000Z";
const EXTRA: &str = "NULL AS ms,NULL AS progress,NULL AS stage,NULL AS max_stage";

fn connector() -> Arc<Connector> {
    Arc::new(
        Connector::new(&culprit_core::ConnectionSpec {
            host: "fixture.invalid".into(),
            database: Some("shop".into()),
            user: Some("unused".into()),
            ..Default::default()
        })
        .unwrap(),
    )
}
fn context(at: &str, uuid: &str, read_only: bool, uptime: u64) -> Vec<Result<Vec<Rec>>> {
    source_context(at, "8.4.0", Rec::from_values(vec![("server_uuid", uuid.into())]), read_only, uptime)
}
fn source_context(at: &str, version: &str, configuration: Rec, read_only: bool, uptime: u64) -> Vec<Result<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::from_values(vec![
            ("database_name", "shop".into()),
            ("server_id", 1u64.into()),
            ("server_name", "fixture".into()),
            ("connection_id", 90u64.into()),
            ("version", version.into()),
            ("login", "reader@%".into()),
            ("performance_schema", 1u64.into()),
            ("read_only", u64::from(read_only).into()),
            ("observed_at", at.into()),
        ])]),
        Ok(vec![configuration]),
        Ok(vec![Rec::from_values(vec![("observed_second", 10_000u64.into()), ("uptime", uptime.into())])]),
        Ok(consumers(true)),
    ]
}
async fn original() -> SessionActionTarget {
    let mut replies = context(AT, UUID, false, 100);
    replies.push(Ok(vec![row(7, "Query", true)]));
    replies.extend(context(LATER, UUID, false, 100));
    let mut reader = Read::with(replies);
    let live = live_rows(&mut reader, &connector(), false, EXTRA).await.unwrap();
    live.target(&live.rows[0]).unwrap()
}
fn attempt(point: Rec, uuid: &str, read_only: bool, uptime: u64, query_limit: u32) -> Read {
    let mut replies = context(AT, UUID, false, 100);
    replies.extend(context(LATER, uuid, read_only, uptime));
    replies.push(Ok(vec![point]));
    replies.push(Ok(vec![]));
    Read::with(replies).with_native_phases().with_query_budget(query_limit, 7)
}
async fn act(reader: &mut Read, target: &SessionActionTarget, action: SessionAction) -> Result<SessionActionOutcome> {
    interrupt_from(reader, &connector(), target, action).await
}
fn point(changes: &[(&str, Value)]) -> Rec {
    let mut changes = changes.to_vec();
    changes.push(("observed_at", "2020-01-01T06:00:00.300000Z".into()));
    row_with(7, "Query", true, &changes)
}

#[tokio::test]
async fn an_original_thread_event_and_start_survive_into_a_bounded_signal_without_a_later_lookup() {
    let target = original().await;
    assert_eq!(target.activity.session.id, "7");
    assert_eq!(target.activity.session.generation, "107");
    assert_eq!(target.activity.request.as_ref().unwrap().id, "3");
    assert_eq!(target.activity.request.as_ref().unwrap().generation, "4");
    let mut reader = attempt(point(&[]), UUID, false, 100, ACTION_QUERIES);
    assert_eq!(act(&mut reader, &target, SessionAction::Cancel).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(reader.query_count(), ACTION_QUERIES);
    assert_eq!(reader.closed, 1);
    assert_eq!(reader.queries.last().unwrap().0, "KILL QUERY 7");
    assert!(reader.queries[reader.queries.len() - 2].0.contains("t.PROCESSLIST_ID=?"));
}

#[tokio::test]
async fn reused_connection_and_advanced_request_generations_are_never_replaced_by_the_latest_row() {
    let target = original().await;
    for changes in [
        vec![("thread_id", 108u64.into())],
        vec![("event_id", 4u64.into())],
        vec![("timer_start", 5u64.into())],
        vec![("end_event_id", 3u64.into())],
    ] {
        let mut reader = attempt(point(&changes), UUID, false, 100, ACTION_QUERIES);
        assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
        assert!(!reader.queries.iter().any(|(sql, _, _)| sql.starts_with("KILL")));
    }
    let mut reader = attempt(point(&[("event_id", 4u64.into())]), UUID, false, 100, ACTION_QUERIES);
    assert_eq!(act(&mut reader, &target, SessionAction::Terminate).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(reader.queries.last().unwrap().0, "KILL CONNECTION 7");
}

#[tokio::test]
async fn member_role_and_native_process_changes_stop_before_signalling() {
    let target = original().await;
    for (uuid, read_only, uptime) in
        [("f54a461c-0001-4000-8000-000000000002", false, 100), (UUID, true, 100), (UUID, false, 101)]
    {
        let mut reader = attempt(point(&[]), uuid, read_only, uptime, ACTION_QUERIES);
        assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
        assert!(!reader.queries.iter().any(|(sql, _, _)| sql.starts_with("KILL")));
    }
}

#[tokio::test]
async fn missing_authority_and_cancelled_native_work_never_fall_back_to_bare_ids() {
    let mut target = original().await;
    target.context.partition = SourcePartition::Target;
    let mut reader = Read::default();
    assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
    assert!(reader.queries.is_empty());
    let target = original().await;
    let mut reader = Read::with(vec![Err(CollectionStop::Cancelled.error())]);
    assert!(act(&mut reader, &target, SessionAction::Cancel).await.is_err());
    assert_eq!(reader.queries.len(), 1);
    let mut reader = attempt(point(&[]), UUID, false, 100, ACTION_QUERIES - 1);
    let error = act(&mut reader, &target, SessionAction::Cancel).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    assert!(!reader.queries.iter().any(|(sql, _, _)| sql.starts_with("KILL")));
}

#[tokio::test]
async fn zero_and_noncanonical_connection_ids_are_refused_before_reading_authority() {
    let original = original().await;
    for id in ["0", "00", "07", " 7", "7 ", "+7", "-7", "7;KILL CONNECTION 90"] {
        let mut target = original.clone();
        target.activity.session.id = id.into();
        let mut reader = Read::default();
        assert!(act(&mut reader, &target, SessionAction::Terminate).await.is_err(), "{id}");
        assert!(reader.queries.is_empty(), "{id}");
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn dispatch_is_attempted_once_and_success_requires_confirmed_native_shutdown() {
    let target = original().await;
    let mut reader = attempt(point(&[]), UUID, false, 100, ACTION_QUERIES);
    reader.shutdown = Some(SourceShutdown::Unconfirmed);
    let error = act(&mut reader, &target, SessionAction::Cancel).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(reader.query_count(), ACTION_QUERIES);
    assert_eq!(reader.queries.iter().filter(|(sql, _, _)| sql.starts_with("KILL")).count(), 1);
    assert_eq!(reader.closed, 1);

    for shutdown in [SourceShutdown::Quiescent, SourceShutdown::Unconfirmed] {
        let mut reader = attempt(point(&[]), UUID, false, 100, ACTION_QUERIES);
        *reader.replies.back_mut().unwrap() = Err(CollectionStop::TimedOut.error());
        reader.shutdown = Some(shutdown);
        let error = act(&mut reader, &target, SessionAction::Cancel).await.unwrap_err();
        let expected = if shutdown == SourceShutdown::Unconfirmed {
            CollectionStop::CancellationUnconfirmed
        } else {
            CollectionStop::TimedOut
        };
        assert_eq!(CollectionStop::from_error(&error), expected);
        assert_eq!(reader.query_count(), ACTION_QUERIES);
        assert_eq!(reader.queries.iter().filter(|(sql, _, _)| sql.starts_with("KILL")).count(), 1);
        assert_eq!(reader.closed, 1);
        assert!(reader.replies.is_empty());
    }
}

#[tokio::test]
async fn production_live_runs_only_its_existing_processlist_read_and_has_no_action_authority() {
    let mut reader = Read::with(vec![Ok(vec![row(7, "Query", true)])]);
    let live = live_rows(&mut reader, &connector(), true, EXTRA).await.unwrap();
    assert_eq!(reader.queries.len(), 1);
    assert!(!reader.queries[0].0.contains("performance_schema"));
    assert_eq!(live.rows.len(), 1);
    assert!(live.target(&live.rows[0]).is_none());
    assert!(live.session_actions().is_empty());
}

#[tokio::test]
async fn live_offers_only_actions_supported_by_the_original_retained_generations() {
    for (command, current, expected) in [
        ("Query", true, vec![SessionAction::Cancel, SessionAction::Terminate]),
        ("Query", false, vec![SessionAction::Terminate]),
        ("Sleep", false, vec![SessionAction::Terminate]),
    ] {
        let mut replies = context(AT, UUID, false, 100);
        replies.push(Ok(vec![row(7, command, current)]));
        replies.extend(context(LATER, UUID, false, 100));
        let mut reader = Read::with(replies);
        let live = live_rows(&mut reader, &connector(), false, EXTRA).await.unwrap();
        assert_eq!(live.session_actions(), expected);
        assert_eq!(reader.queries.len(), 9);
    }
}

#[tokio::test]
async fn unavailable_instrumentation_preserves_diagnostics_and_terminal_errors_do_not_start_a_fallback() {
    let denied = culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied");
    let mut reader = Read::with(vec![Err(denied), Ok(vec![row(7, "Query", true)])]);
    let live = live_rows(&mut reader, &connector(), false, EXTRA).await.unwrap();
    assert_eq!(live.rows.len(), 1);
    assert!(live.target(&live.rows[0]).is_none());
    assert!(live.session_actions().is_empty());
    assert_eq!(reader.queries.len(), 2);
    let mut reader = Read::with(vec![Err(CollectionStop::TimedOut.error())]);
    assert!(live_rows(&mut reader, &connector(), false, EXTRA).await.is_err());
    assert_eq!(reader.queries.len(), 1);
}

#[tokio::test]
async fn mariadb_native_uid_and_request_generations_use_the_same_twenty_phase_owner() {
    let authority = |at| {
        source_context(at, "11.4.3-MariaDB", Rec::from_values(vec![("server_uid", MARIADB_UID.into())]), false, 100)
    };
    let mut replies = authority(AT);
    replies.push(Ok(vec![row(7, "Query", true)]));
    replies.extend(authority(LATER));
    let mut reader = Read::with(replies);
    let live = live_rows(&mut reader, &connector(), false, EXTRA).await.unwrap();
    let target = live.target(&live.rows[0]).unwrap();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &target.context.partition else {
        panic!("native MariaDB member required")
    };
    assert_eq!(member.namespace.id, "mariadb-server-uid");
    assert_eq!(member.native.id, MARIADB_UID);
    assert_eq!(target.activity.session.generation, "107");
    assert_eq!(target.activity.request.as_ref().unwrap().generation, "4");

    let mut replies = authority(AT);
    replies.extend(authority(LATER));
    replies.push(Ok(vec![point(&[])]));
    replies.push(Ok(vec![]));
    let mut reader = Read::with(replies).with_native_phases().with_query_budget(ACTION_QUERIES, 7);
    assert_eq!(act(&mut reader, &target, SessionAction::Cancel).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(reader.query_count(), ACTION_QUERIES);
    assert_eq!(reader.closed, 1);
    assert_eq!(reader.queries.iter().filter(|(sql, _, _)| sql.contains("@@GLOBAL.server_uid")).count(), 2);
    assert!(!reader.queries.iter().any(|(sql, _, _)| sql.contains("server_uuid")));
    assert_eq!(reader.queries.last().unwrap().0, "KILL QUERY 7");
}

#[tokio::test]
async fn unsupported_or_denied_mariadb_uid_keeps_diagnostics_without_action_authority() {
    for (version, uid_denied) in [("11.4.2-MariaDB", false), ("11.4.3-MariaDB", true)] {
        let mut replies =
            source_context(AT, version, Rec::from_values(vec![("server_uid", MARIADB_UID.into())]), false, 100);
        replies.truncate(1);
        if uid_denied {
            replies.push(Err(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied")));
        }
        replies.push(Ok(vec![row(7, "Query", true)]));
        let mut reader = Read::with(replies);
        let live = live_rows(&mut reader, &connector(), false, EXTRA).await.unwrap();
        assert_eq!(live.rows.len(), 1);
        assert!(live.target(&live.rows[0]).is_none());
        assert!(live.session_actions().is_empty());
        assert_eq!(reader.queries.len(), if uid_denied { 3 } else { 2 });
        assert!(!reader.queries.last().unwrap().0.contains("performance_schema"));
        assert_eq!(
            reader.queries.iter().filter(|(sql, _, _)| sql.contains("@@GLOBAL.server_uid")).count(),
            usize::from(uid_denied)
        );
    }
}

#[tokio::test]
async fn system_sentinel_and_foreign_database_rows_never_gain_actions() {
    let target = original().await;
    for changes in [
        vec![("thread_id", Value::NULL)],
        vec![("thread_id", 0u64.into())],
        vec![("user_name", "system user".into())],
        vec![("command", "Daemon".into())],
        vec![("request_db", "other_database".into())],
    ] {
        assert!(observed_target(&point(&changes), &target.context, true).unwrap().is_none());
    }
}
