use super::super::super::activity::tests::native_session_context;
use super::*;

const LOGIN: &str = "2020-01-01T07:00:00.1533333";

#[test]
fn a_canonical_negative_peer_prevents_a_positive_mars_row_from_creating_an_episode_or_focus() {
    for lifecycle in [SessionLifecycle::Unknown, SessionLifecycle::Connecting, SessionLifecycle::PoolResetPrelogin] {
        let positive = actor();
        let mut peer = positive.clone();
        peer.key.request.as_mut().unwrap().id = "1".into();
        peer.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        let original = frame(2, vec![positive.clone(), peer], true);
        let retained = original
            .2
            .iter()
            .map(|(reference, observation)| ActivityFrameRow { observation, reference: Some(reference) })
            .collect::<Vec<_>>();
        let full = ActivityFrameEvidence::new(&original.0, &retained, true).unwrap();
        assert!(positive.has_session_generation_authority());
        let update = observe(&original, None);
        let negative = lifecycle != SessionLifecycle::Unknown;
        assert_eq!(update.checkpoint.episodes.is_empty(), negative);
        assert_eq!(update.focuses.is_empty(), negative);
        assert_eq!(full.page_wait_task_requests().unwrap().is_empty(), negative);
        if negative {
            let previous = observe(&frame(1, vec![positive], true), None);
            let later = observe(&original, Some(&previous.checkpoint));
            assert_eq!(later.checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Uncertain);
            assert_eq!(later.checkpoint.episodes[0].latest, previous.checkpoint.episodes[0].latest);
        }
    }
}

#[test]
fn canonical_saved_idle_transaction_rows_do_not_create_generation_authority_or_durable_incidents() {
    for (state, login) in
        [("Sleeping", LOGIN), ("Dormant", LOGIN), ("Preconnect", LOGIN), ("Sleeping", "1900-01-01T00:00:00.0000000")]
    {
        let mut head = actor();
        head.key.request = None;
        head.key.transaction = Some("native-transaction".into());
        head.statement_identity = None;
        head.request_counters = None;
        head.idle = true;
        head.scope = ActivityScopeBasis::TransactionDatabase;
        head.open_transactions = Some(1);
        head.wait = None;
        native_session_context(&mut head, state, login);
        let mut waiter = lock_wait_actor();
        waiter.blocking_status = ActivityBlockingStatus::Known;
        waiter.key.session.id = "52".into();
        waiter.wait.as_mut().unwrap().elapsed_ms = Some("40000".into());
        waiter.wait.as_mut().unwrap().owners = vec![ActivityWaitOwner::Session {
            id: head.key.session.id.clone(),
            generation: Some(head.key.session.generation.clone()),
        }];
        let original = frame(1, vec![head.clone(), waiter], true);
        original.2[0].0.validate_canonical_record(&original.0, &TelemetryRecord::from(head.clone())).unwrap();
        let update = observe(&original, None);
        assert_eq!(
            update.checkpoint.episodes.iter().any(|episode| episode.signal.activity_key() == Some(&head.key)),
            head.has_session_generation_authority()
        );
        assert!(update.focuses.iter().all(|(_, focus)| {
            focus.activity_participant().is_none_or(|participant| participant.original.key != head.key)
        }));
    }
}

#[test]
fn canonical_mars_rows_with_known_prelogin_lifecycle_cannot_enroll_requests_or_page_task_rosters() {
    for lifecycle in [SessionLifecycle::PoolResetPrelogin, SessionLifecycle::Connecting, SessionLifecycle::Unknown] {
        let mut first = actor();
        native_session_context(&mut first, "Running", LOGIN);
        let context = first.session_context.as_deref_mut().unwrap();
        context.native = None;
        context.lifecycle = lifecycle;
        let mut second = first.clone();
        second.key.request.as_mut().unwrap().id = "1".into();
        let original = frame(1, vec![first, second], true);
        let borrowed = original
            .2
            .iter()
            .map(|(reference, observation)| ActivityFrameRow { observation, reference: Some(reference) })
            .collect::<Vec<_>>();
        let retained = ActivityFrameEvidence::new(&original.0, &borrowed, true).unwrap();
        assert_eq!(retained.rows().count(), 2, "both original MARS positions remain visible");
        let update = observe(&original, None);
        let known_negative = lifecycle != SessionLifecycle::Unknown;
        assert_eq!(update.checkpoint.episodes.is_empty(), known_negative);
        assert_eq!(update.focuses.is_empty(), known_negative);
        assert_eq!(retained.page_wait_task_requests().unwrap().is_empty(), known_negative);
    }
}

#[test]
fn a_known_negative_row_keeps_previous_incident_history_uncertain_without_refreshing_or_recovering_it() {
    let first = observe(&frame(1, vec![actor()], true), None);
    let mut negative = actor();
    negative.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle: SessionLifecycle::PoolResetPrelogin,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: None,
    }));
    let later = frame(2, vec![negative], true);
    let update = observe(&later, Some(&first.checkpoint));
    let previous = &first.checkpoint.episodes[0];
    let retained = &update.checkpoint.episodes[0];
    assert_eq!(retained.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(retained.first, previous.first);
    assert_eq!(retained.latest, previous.latest);
    assert_eq!(retained.last_seen_at, previous.last_seen_at);
    assert!(retained.recovery.is_none());
    assert!(update.focuses.is_empty());
}
