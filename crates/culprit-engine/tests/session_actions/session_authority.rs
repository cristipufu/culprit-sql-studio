use super::*;
use culprit_core::{blocking::HeadKind, telemetry::*};

fn recorded_live(lifecycle: SessionLifecycle) -> LiveState {
    let mut census: ActivityCensusRead = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/session-census.native.json"
    )))
    .unwrap();
    let context = support::target(Engine::Postgres, "42").context;
    census.source.target = context.target;
    census.source.partition = context.partition;
    let mut head = census.observations.remove(0);
    head.key.session.id = "42".into();
    head.key.transaction = Some("native-transaction".into());
    head.open_transactions = Some(1);
    head.database = Some("shop".into());
    let session = head.session_context.as_deref_mut().unwrap();
    session.native = None;
    session.lifecycle = lifecycle;
    let mut waiter = head.clone();
    waiter.key.session.id = "43".into();
    waiter.key.transaction = None;
    waiter.key.request =
        Some(NativeActivityIdentity { id: "1".into(), generation: "native-request-generation".into() });
    waiter.idle = false;
    waiter.scope = ActivityScopeBasis::RequestDatabase;
    waiter.request_counters = Some(ActivityCounters::default());
    waiter.session_context = None;
    waiter.wait = Some(ActivityWait {
        class: None,
        native_type: Some("native-lock".into()),
        identified_resources: vec![],
        elapsed_ms: Some("40000".into()),
        owners: vec![ActivityWaitOwner::Session {
            id: head.key.session.id.clone(),
            generation: Some(head.key.session.generation.clone()),
        }],
        resources: None,
    });
    census.observations = vec![head, waiter];
    let SourceObservation::Activity(native) = &mut census.source.observation else { panic!() };
    native.epoch = census.source.partition.native_epoch().map(str::to_owned);
    native.native_rows = 2;
    native.observations = 2;
    native.coverage.records = 2;
    census.frame().unwrap();
    let mut live = support::live(Engine::Postgres, "42");
    live.activity_census = Some(Box::new(census));
    let head = &mut live.sessions[0];
    head.state = "idle".into();
    head.transaction_ms = Some(300000.0);
    head.action_target = None;
    let mut waiter = head.clone();
    waiter.id = "43".into();
    waiter.state = "active".into();
    waiter.blocked_by = Some("42".into());
    waiter.transaction_ms = None;
    waiter.wait = Some("native-lock".into());
    waiter.wait_ms = Some(40000.0);
    live.sessions.push(waiter);
    live.session_actions.clear();
    live
}

#[tokio::test]
async fn live_blocking_keeps_census_rows_but_cannot_borrow_an_unestablished_session_as_the_head() {
    for lifecycle in [SessionLifecycle::Idle, SessionLifecycle::PoolResetPrelogin, SessionLifecycle::Connecting] {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let original = recorded_live(lifecycle);
        *state.live_override.lock().unwrap() = Some(original.clone());
        let workspace = workspace(directory.path(), &state);
        add(&workspace, Environment::Development, AuthKind::None);
        let live = workspace.live("pilot").await.unwrap();
        assert_eq!(live.activity_census, original.activity_census);
        let mut expected_sessions = original.sessions.clone();
        for session in &mut expected_sessions {
            // This fixture has no exact current-request key for the session-only progress endpoint.
            session.progress_eligible = Some(false);
        }
        assert_eq!(live.sessions, expected_sessions, "raw live rows remain visible with derived progress eligibility");
        assert_eq!(live.session_census.as_ref().unwrap().observed_rows, 2);
        let head = &live.blocking.as_ref().unwrap().chains[0];
        assert_eq!(head.head, "42");
        let established = lifecycle == SessionLifecycle::Idle;
        assert_eq!(head.tree.context.is_some(), established);
        assert_eq!(head.head_kind, if established { HeadKind::IdleInTransaction } else { HeadKind::Unknown });
    }
}
