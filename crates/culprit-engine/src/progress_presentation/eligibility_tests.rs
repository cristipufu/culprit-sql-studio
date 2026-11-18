use super::*;
use culprit_core::LiveState;
use culprit_core::telemetry::*;
use serde_json::json;

fn qualify(live: &mut LiveState) {
    let frame = live.activity_census.as_deref().map(|read| read.frame().unwrap());
    qualify_live(&mut live.sessions, frame.as_ref());
}

fn current_live(lifecycle: SessionLifecycle) -> LiveState {
    let mut read: ActivityCensusRead = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/session-census.native.json"
    )))
    .unwrap();
    let row = &mut read.observations[0];
    row.idle = false;
    row.scope = ActivityScopeBasis::RequestDatabase;
    row.key.request = Some(NativeActivityIdentity { id: "0".into(), generation: "native-request-generation".into() });
    row.request_counters = Some(ActivityCounters::default());
    let context = row.session_context.as_deref_mut().unwrap();
    context.native = None;
    context.lifecycle = lifecycle;
    let session: culprit_core::Session = serde_json::from_value(json!({
        "id":row.key.session.id,"activity_key":row.key,"state":"running","progress_eligible":true,
    }))
    .unwrap();
    read.frame().unwrap();
    LiveState {
        activity_census: Some(Box::new(read)),
        session_census: None,
        notes: vec![],
        taken_at: "2020-01-01T12:00:00Z".into(),
        sessions: vec![session],
        waits: vec![],
        session_actions: vec![],
        session_actions_note: None,
        session_action_route: None,
        blocking: None,
    }
}

#[test]
fn known_negative_current_metadata_overrides_injected_presentation_eligibility() {
    for lifecycle in [
        SessionLifecycle::Running,
        SessionLifecycle::Unknown,
        SessionLifecycle::PoolResetPrelogin,
        SessionLifecycle::Connecting,
    ] {
        let mut live = current_live(lifecycle);
        qualify(&mut live);
        assert_eq!(
            live.sessions[0].progress_eligible,
            Some(matches!(lifecycle, SessionLifecycle::Running | SessionLifecycle::Unknown))
        );
        let original = live.activity_census.as_deref().unwrap();
        assert_eq!(original.observations[0].session_context.as_deref().unwrap().lifecycle, lifecycle);
    }
}

#[test]
fn exact_current_keys_and_all_recorded_mars_requests_are_required_without_a_representative() {
    let mut live = current_live(SessionLifecycle::Running);
    let read = live.activity_census.as_deref_mut().unwrap();
    let mut parallel = read.observations[0].clone();
    parallel.key.request.as_mut().unwrap().id = "1".into();
    read.observations.push(parallel);
    let SourceObservation::Activity(native) = &mut read.source.observation else { panic!() };
    native.native_rows = 2;
    native.observations = 2;
    native.coverage.records = 2;
    read.frame().unwrap();
    qualify(&mut live);
    assert_eq!(live.sessions[0].progress_eligible, Some(false), "a display prefix cannot hide a native MARS request");
    let mut live = current_live(SessionLifecycle::Running);
    live.sessions.push(live.sessions[0].clone());
    qualify(&mut live);
    assert!(live.sessions.iter().all(|row| row.progress_eligible == Some(false)));
    for missing_key in [false, true] {
        let mut live = current_live(SessionLifecycle::Running);
        if missing_key {
            live.sessions[0].activity_key = None;
        } else {
            live.sessions[0].activity_key.as_mut().unwrap().session.generation = "another-login".into();
        }
        qualify(&mut live);
        assert_eq!(live.sessions[0].progress_eligible, Some(false));
    }
    let mut live = current_live(SessionLifecycle::Running);
    live.sessions[0].id = "another-display-id".into();
    qualify(&mut live);
    assert_eq!(live.sessions[0].progress_eligible, Some(false));
}

#[test]
fn unacquired_metadata_is_unknown_and_cannot_preserve_a_provider_supplied_eligibility_verdict() {
    for supplied in [false, true] {
        let mut live = current_live(SessionLifecycle::Running);
        live.activity_census = None;
        live.sessions[0].progress_eligible = Some(supplied);
        qualify(&mut live);
        assert_eq!(live.sessions[0].progress_eligible, None);
    }
}

#[test]
fn a_negative_peer_clears_action_targets_for_the_exact_whole_identity_but_not_another_generation() {
    for lifecycle in [SessionLifecycle::Unknown, SessionLifecycle::Connecting] {
        let mut live = current_live(SessionLifecycle::Running);
        let read = live.activity_census.as_deref_mut().unwrap();
        let mut peer = read.observations[0].clone();
        peer.key.request.as_mut().unwrap().id = "1".into();
        peer.session_context.as_deref_mut().unwrap().lifecycle = lifecycle;
        read.observations.push(peer);
        read.source.partition = SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("owned-test-machine"),
                native: NativeIdentity::new("owned-test-member"),
            }),
            epoch: Some("native-source-generation".into()),
        };
        let SourceObservation::Activity(native) = &mut read.source.observation else { panic!() };
        native.native_rows = 2;
        native.observations = 2;
        native.coverage.records = 2;
        native.database_writable = Some(true);
        let target = culprit_core::SessionActionTarget {
            context: NativeReadContext::from_activity(&read.source).unwrap(),
            activity: read.observations[0].key.clone(),
            statement: None,
        };
        assert!(target.is_valid_for(culprit_core::SessionAction::Terminate));
        live.sessions[0].action_target = Some(target.clone());
        let mut unrelated = live.sessions[0].clone();
        unrelated.id = "another-generation".into();
        unrelated.action_target.as_mut().unwrap().activity.session.generation = "another-native-generation".into();
        live.sessions.push(unrelated);
        qualify(&mut live);
        assert_eq!(live.sessions[0].action_target.is_none(), lifecycle == SessionLifecycle::Connecting);
        assert!(live.sessions[1].action_target.is_some(), "a refusal cannot join another generation by displayedID");
        assert_eq!(live.activity_census.as_deref().unwrap().observations.len(), 2);
    }
}
