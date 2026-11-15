use super::*;
use crate::{
    live::tests::support::{authority, platform, session, signalled, target},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn ready() -> FakeRead {
    FakeRead::new(vec![
        Ok(vec![platform()]),
        Ok(vec![authority()]),
        Ok(vec![session()]),
        Ok(vec![authority()]),
        Ok(vec![signalled(true)]),
    ])
}

async fn terminate(reader: &mut impl NativeRead) -> Result<SessionActionOutcome> {
    interrupt_from(reader, "fixture.invalid", 1433, &target(), SessionAction::Terminate).await
}

fn no_kill(reader: &FakeRead) {
    assert!(reader.calls.iter().all(|call| !call.sql.contains("KILL ")));
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn one_held_native_owner_rechecks_original_generation_and_returns_only_signal_sent() {
    let mut reader = ready();
    assert_eq!(terminate(&mut reader).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(reader.calls.len(), 5);
    assert_eq!(reader.closed, 1);
    let dispatch = &reader.calls[4];
    assert_eq!(dispatch.limit, 1);
    assert!(dispatch.sql.contains("KILL 42;"));
    assert!(dispatch.sql.contains("login_time=CONVERT(datetime,@P2,126)"));
    assert!(dispatch.sql.contains("session_id<>@@SPID"));
    assert!(dispatch.sql.contains("DATABASEPROPERTYEX"));
    assert!(
        matches!(&dispatch.parameters[1], Parameter::Text(value) if value == &target().activity.session.generation)
    );
}

#[tokio::test]
async fn a_missing_recycled_or_ambiguous_spid_never_reaches_kill_or_retries() {
    let replacement = replace(&session(), &[("session_generation", Val::Text("2020-01-01T07:00:01".into()))]);
    for rows in [vec![], vec![replacement], vec![session(), session()]] {
        let mut reader = FakeRead::new(vec![Ok(vec![platform()]), Ok(vec![authority()]), Ok(rows)]);
        assert!(terminate(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 3);
        no_kill(&reader);
    }
}

#[tokio::test]
async fn changed_database_member_role_or_process_epoch_never_reaches_kill() {
    for (field, value) in [
        ("replica_id", Val::Text("11111111-2222-3333-4444-555555555555".into())),
        ("database_name", Val::Text("another_database".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("local_started_ticks", Val::Int(200)),
    ] {
        let changed = replace(&authority(), &[(field, value)]);
        let mut reader =
            FakeRead::new(vec![Ok(vec![platform()]), Ok(vec![authority()]), Ok(vec![session()]), Ok(vec![changed])]);
        let error = terminate(&mut reader).await.unwrap_err();
        assert!(matches!(error.detail, Some(culprit_core::ErrorDetail::SessionActionChanged)), "{field}");
        no_kill(&reader);
    }
}

#[tokio::test]
async fn already_changed_or_unknown_source_authority_stops_before_target_or_kill_queries() {
    for (field, value) in [
        ("replica_id", Val::Text("11111111-2222-3333-4444-555555555555".into())),
        ("replica_id", Val::Null),
        ("local_epoch_visible", Val::Bool(false)),
    ] {
        let changed = if field == "local_epoch_visible" {
            replace(
                &authority(),
                &[
                    (field, value),
                    ("local_process_id", Val::Null),
                    ("local_started_ticks", Val::Null),
                    ("local_now_ticks", Val::Null),
                    ("local_started_without_offset", Val::Null),
                ],
            )
        } else {
            replace(&authority(), &[(field, value)])
        };
        let mut reader = FakeRead::new(vec![Ok(vec![platform()]), Ok(vec![changed])]);
        assert!(terminate(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 2);
        no_kill(&reader);
    }
}

#[tokio::test]
async fn self_system_and_other_culprit_sessions_remain_refused() {
    for change in
        [("own", Val::Bool(true)), ("user_process", Val::Bool(false)), ("program_name", Val::Text("Culprit".into()))]
    {
        let mut reader =
            FakeRead::new(vec![Ok(vec![platform()]), Ok(vec![authority()]), Ok(vec![replace(&session(), &[change])])]);
        assert!(terminate(&mut reader).await.is_err());
        no_kill(&reader);
    }
}

#[tokio::test]
async fn no_cancel_or_malformed_target_can_start_native_work() {
    let mut reader = FakeRead::new(vec![]);
    let error =
        interrupt_from(&mut reader, "fixture.invalid", 1433, &target(), SessionAction::Cancel).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
    no_kill(&reader);
    let mut malformed = target();
    malformed.activity.session.id = "42; KILL 43".into();
    let mut reader = FakeRead::new(vec![]);
    assert!(interrupt_from(&mut reader, "fixture.invalid", 1433, &malformed, SessionAction::Terminate).await.is_err());
    assert!(reader.calls.is_empty());
    no_kill(&reader);
    let mut unknown_role = target();
    unknown_role.context.database_writable = None;
    let mut reader = FakeRead::new(vec![]);
    assert!(
        interrupt_from(&mut reader, "fixture.invalid", 1433, &unknown_role, SessionAction::Terminate).await.is_err()
    );
    assert!(reader.calls.is_empty());
    no_kill(&reader);
}

#[tokio::test]
async fn native_dispatch_failure_or_unconfirmed_close_never_reports_signal_sent_or_retries() {
    let mut denied = FakeRead::new(vec![
        Ok(vec![platform()]),
        Ok(vec![authority()]),
        Ok(vec![session()]),
        Ok(vec![authority()]),
        Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture"))),
    ]);
    assert_eq!(terminate(&mut denied).await.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(denied.calls.len(), 5);
    assert_eq!(denied.closed, 1);
    let mut uncertain = ready();
    uncertain.shutdown = SourceShutdown::Unconfirmed;
    let error = terminate(&mut uncertain).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(uncertain.calls.len(), 5);
    assert_eq!(uncertain.closed, 1);
}

#[tokio::test]
async fn terminal_cancellation_and_budget_failures_do_not_replace_the_owner_or_replay_a_signal() {
    let mut capped = ready().with_query_limit(4);
    let error = terminate(&mut capped).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    assert_eq!(capped.calls.len(), 4);
    no_kill(&capped);
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed] {
        let mut interrupted = FakeRead::new(vec![
            Ok(vec![platform()]),
            Ok(vec![authority()]),
            Ok(vec![session()]),
            Ok(vec![authority()]),
            Err(ReadError::Stopped(stop)),
        ]);
        assert_eq!(CollectionStop::from_error(&terminate(&mut interrupted).await.unwrap_err()), stop);
        assert_eq!(interrupted.calls.len(), 5, "a possibly dispatched command is never retried");
        assert_eq!(interrupted.closed, 1);
    }
}

struct ChangedConnection {
    inner: FakeRead,
    generation: u64,
}
#[async_trait::async_trait]
impl NativeRead for ChangedConnection {
    async fn query(
        &mut self,
        sql: &str,
        parameters: Vec<Parameter>,
        limit: usize,
    ) -> crate::read::ReadResult<Vec<crate::value::Rec>> {
        if self.inner.calls.len() == 3 {
            self.generation += 1;
        }
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(self.generation)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}

#[tokio::test]
async fn an_equal_native_tuple_on_a_replaced_control_connection_cannot_authorize_kill() {
    let mut reader = ChangedConnection { inner: ready(), generation: 1 };
    let error = terminate(&mut reader).await.unwrap_err();
    assert!(matches!(error.detail, Some(culprit_core::ErrorDetail::SessionActionChanged)));
    assert_eq!(reader.inner.calls.len(), 4);
    no_kill(&reader.inner);
}

#[tokio::test]
async fn boxed_native_owner_checks_and_dispatch_fit_the_same_eight_query_budget() {
    use crate::instance::test_support::{epoch, metadata};
    let database = replace(&crate::activity::database_context::tests::row(3), &[("replica_id", Val::Null)]);
    let mut original = FakeRead::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![database.clone()])]);
    let context = Context::read(&mut original, "fixture.invalid", 1433).await.unwrap();
    let mut target = target();
    target.context = context.completed(&context).unwrap();
    let mut reader = FakeRead::new(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![database.clone()]),
        Ok(vec![session()]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![database]),
        Ok(vec![signalled(true)]),
    ])
    .with_query_limit(ACTION_QUERIES);
    assert_eq!(
        interrupt_from(&mut reader, "fixture.invalid", 1433, &target, SessionAction::Terminate).await.unwrap(),
        SessionActionOutcome::SignalSent
    );
    assert_eq!(reader.calls.len(), ACTION_QUERIES as usize);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn a_late_guard_rejection_withholds_success_but_a_completed_signal_is_not_an_atomicity_claim() {
    let mut changed = FakeRead::new(vec![
        Ok(vec![platform()]),
        Ok(vec![authority()]),
        Ok(vec![session()]),
        Ok(vec![authority()]),
        Ok(vec![signalled(false)]),
    ]);
    assert!(terminate(&mut changed).await.is_err());
    assert_eq!(changed.calls.len(), 5);
    // SQL Server accepts only a SPID: a generation can change after its last predicate. This test's
    // scripted acknowledgement proves dispatch handling only, never atomic targeting or rollback completion.
    let mut accepted = ready();
    assert_eq!(terminate(&mut accepted).await.unwrap(), SessionActionOutcome::SignalSent);
}
