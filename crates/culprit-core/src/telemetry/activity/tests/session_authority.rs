use super::*;
use crate::{blocking, telemetry::activity_frame::tests as frames};

const LOGIN: &str = "2020-01-01T07:00:00.1533333";

#[test]
fn a_supplied_population_projects_only_the_existing_indexs_recorded_generation_refusals() {
    for (lifecycle, refused) in [
        (SessionLifecycle::Running, false),
        (SessionLifecycle::Unknown, false),
        (SessionLifecycle::Connecting, true),
        (SessionLifecycle::PoolResetPrelogin, true),
    ] {
        let original = request("42");
        let mut peer = original.clone();
        peer.key.request.as_mut().unwrap().id = "1".into();
        peer.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        let rows = [original.clone(), peer, request("43")];
        let before = rows.clone();
        let disqualified = ActivityEvidence::disqualified_session_generations(&rows).unwrap();
        let expected = if refused { BTreeSet::from([original.key.session]) } else { BTreeSet::new() };
        assert_eq!(disqualified, expected, "{lifecycle:?}");
        assert_eq!(rows, before, "original points are not filtered or rewritten");
    }
    let original = request("42");
    assert!(ActivityEvidence::disqualified_session_generations([&original, &original]).is_err());
    let mut foreign_generation = original.clone();
    foreign_generation.key.session.generation = "another-login".into();
    assert!(ActivityEvidence::disqualified_session_generations([&original, &foreign_generation]).is_err());
    let mut malformed_unrelated = request("43");
    malformed_unrelated.key.session.id.clear();
    assert!(ActivityEvidence::disqualified_session_generations([&original, &malformed_unrelated]).is_err());
    assert!(ActivityEvidence::disqualified_session_generations(std::iter::empty()).unwrap().is_empty());
}

#[test]
fn a_negative_mars_peer_disqualifies_the_whole_generation_before_any_selection_or_traversal() {
    for lifecycle in [SessionLifecycle::Unknown, SessionLifecycle::Connecting, SessionLifecycle::PoolResetPrelogin] {
        let mut positive = request("42");
        positive.statement_identity = Some(NativeIdentity::new("statement"));
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
        blocked(&mut positive, vec![owner("43")]);
        let tail = idle("43");
        let rows = [positive.clone(), peer, tail.clone()];
        let source = frames::source(rows.len());
        let refs = frames::references(&source, &rows);
        let frame = ActivityFrameEvidence::new(&source, &frames::borrowed(&rows, &refs), true).unwrap();
        let negative = lifecycle != SessionLifecycle::Unknown;
        assert_eq!(frame.rows().count(), 3);
        assert_eq!(frame.has_session_generation_authority(&positive.key.session), !negative);
        assert_eq!(frame.session_generation_is_disqualified(&positive.key.session), negative);
        let selected = ActivityEvidence::select_frame(&frame, &ActivityPolicy::default()).unwrap();
        assert_eq!(selected.conditions.contains_key(&positive.key), !negative);
        assert_eq!(selected.conditions.contains_key(&tail.key), !negative);
        assert_eq!(frame.page_wait_task_requests().unwrap().is_empty(), negative);
        let mut waiter = request("44");
        blocked(&mut waiter, vec![owner("42")]);
        let all = [rows.to_vec(), vec![waiter]].concat();
        let edges = ActivityEvidence::blocking_observations(&all).unwrap();
        let owner_edge = edges.iter().find(|edge| edge.waiter.session.id == "44").unwrap();
        assert_eq!(owner_edge.owner_observed, !negative);
    }
}

#[test]
fn recorded_negative_context_withdraws_authority_without_invalidating_or_rewriting_native_rows() {
    for (state, login) in [("Dormant", LOGIN), ("Preconnect", LOGIN), ("Sleeping", "1900-01-01T00:00:00.0000000")] {
        let mut row = idle("42");
        native_session_context(&mut row, state, login);
        assert!(row.is_valid(), "{state}");
        assert!(!row.has_session_generation_authority(), "{state}");
        let restored: ActivityObservation = serde_json::from_slice(&serde_json::to_vec(&row).unwrap()).unwrap();
        assert_eq!(restored, row);
        assert!(!restored.has_session_generation_authority());
    }
    let mut row = request("42");
    assert!(row.has_session_generation_authority(), "unacquired metadata preserves the native contract");
    native_session_context(&mut row, "Running", LOGIN);
    assert!(row.is_valid() && row.has_session_generation_authority());
    row.session_context.as_deref_mut().unwrap().native = None;
    row.session_context.as_deref_mut().unwrap().lifecycle = SessionLifecycle::Unknown;
    assert!(row.is_valid() && row.has_session_generation_authority());
    for lifecycle in [SessionLifecycle::PoolResetPrelogin, SessionLifecycle::Connecting] {
        row.session_context.as_deref_mut().unwrap().lifecycle = lifecycle;
        assert!(row.is_valid(), "native detail is independently unavailable");
        assert!(!row.has_session_generation_authority());
        assert!(row.conditions(&ActivityPolicy::default()).is_empty());
    }
}

#[test]
fn a_canonical_idle_transaction_keeps_topology_but_cannot_supply_blocker_context_or_an_idle_incident() {
    for (state, login) in
        [("Sleeping", LOGIN), ("Dormant", LOGIN), ("Preconnect", LOGIN), ("Sleeping", "1900-01-01T00:00:00.0000000")]
    {
        let mut head = idle("42");
        native_session_context(&mut head, state, login);
        head.statement = DefinitionPayload::sql("UPDATE sample SET amount = 4", crate::Engine::Mssql);
        let mut waiter = request("43");
        blocked(
            &mut waiter,
            vec![ActivityWaitOwner::Session {
                id: head.key.session.id.clone(),
                generation: Some(head.key.session.generation.clone()),
            }],
        );
        let rows = [head.clone(), waiter];
        let mut source = frames::source(rows.len());
        source.partition = super::super::super::native_read_context::tests::read_context().partition;
        let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
        native.epoch = source.partition.native_epoch().map(str::to_owned);
        let refs = frames::references(&source, &rows);
        let frame = ActivityFrameEvidence::new(&source, &frames::borrowed(&rows, &refs), true).unwrap();
        assert_eq!(frame.row(&head.key).unwrap().observation, &head, "original census body remains intact");
        let selected = ActivityEvidence::select(&rows, &ActivityPolicy::default()).unwrap();
        let established = head.has_session_generation_authority();
        assert_eq!(selected.blocking[0].owner_observed, established);
        assert_eq!(selected.conditions.contains_key(&head.key), established);
        let report = blocking::analyze_activity(&frame).unwrap().unwrap();
        let chain = &report.chains[0];
        assert_eq!(chain.head, "42");
        assert_eq!(
            chain.head_kind,
            if established { blocking::HeadKind::IdleInTransaction } else { blocking::HeadKind::Unknown }
        );
        assert_eq!(chain.tree.context.is_some(), established);
        assert_eq!(chain.tree.children.len(), 1);
        assert_eq!(chain.tree.children[0].session, "43");
    }
}

#[test]
fn a_known_unestablished_middle_row_cannot_transfer_blocking_authority_to_another_idle_transaction() {
    for middle_has_request in [false, true] {
        for lifecycle in [SessionLifecycle::Idle, SessionLifecycle::Connecting, SessionLifecycle::PoolResetPrelogin] {
            let mut waiter = request("1");
            blocked(&mut waiter, vec![owner("2")]);
            let mut middle = request("2");
            if !middle_has_request {
                middle.key.request = None;
                middle.request_counters = None;
                middle.idle = true;
            }
            middle.session_context = Some(Box::new(ActivitySessionContext {
                lifecycle,
                login_age_ms: None,
                last_request_end_age_ms: None,
                client_library: None,
                client_process_id: None,
                native: None,
            }));
            blocked(&mut middle, vec![owner("3")]);
            let head = idle("3");
            let rows = [waiter, middle.clone(), head.clone()];
            let evidence = ActivityEvidence::select(&rows, &ActivityPolicy::default()).unwrap();
            let established = lifecycle == SessionLifecycle::Idle;
            assert!(evidence.observations.iter().any(|row| row.key == middle.key), "raw middle context is retained");
            assert_eq!(evidence.conditions.contains_key(&head.key), established);
            assert_eq!(
                ActivityEvidence::context_for(&rows, &BTreeSet::from([rows[0].key.clone()]))
                    .unwrap()
                    .contains(&head.key),
                established
            );
            let edges = ActivityEvidence::blocking_observations(&rows).unwrap();
            assert_eq!(edges.iter().any(|edge| edge.waiter == middle.key), middle_has_request && established);
        }
    }
}

#[test]
fn borrowed_generation_refusals_preserve_repeated_workers_without_claiming_a_complete_activity_frame() {
    let positive = request("42");
    let mut negative = positive.clone();
    negative.key.request.as_mut().unwrap().id = "1".into();
    negative.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle: SessionLifecycle::PoolResetPrelogin,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: None,
    }));
    let observed = ActivityEvidence::observed_session_generation_refusals([&positive, &positive, &negative]).unwrap();
    assert_eq!(observed, BTreeSet::from([positive.key.session.clone()]));
    negative.key.session.generation.push_str("-other-generation");
    let other = ActivityEvidence::observed_session_generation_refusals([&positive, &positive, &negative]).unwrap();
    assert!(!other.contains(&positive.key.session));
    assert!(other.contains(&negative.key.session));
    assert!(ActivityEvidence::observed_session_generation_refusals([&positive, &positive]).unwrap().is_empty());
    assert!(ActivityEvidence::disqualified_session_generations([&positive, &positive]).is_err());
    let mut invalid = positive.clone();
    invalid.key.session.id.clear();
    assert!(ActivityEvidence::observed_session_generation_refusals([&positive, &invalid]).is_err());
}
