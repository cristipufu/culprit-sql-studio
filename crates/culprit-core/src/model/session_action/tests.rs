use super::*;
use crate::DatabaseIdentity;
use crate::telemetry::{NativeActivityIdentity, SharedResourceIdentity, SharedResourceKind, SourcePartition};

fn target() -> SessionActionTarget {
    SessionActionTarget {
        context: NativeReadContext {
            target: DatabaseIdentity::server(Engine::Postgres, "orders", "fixture.invalid", 5432, None).into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Postgres,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-cluster"),
                    native: NativeIdentity::new("native-member"),
                }),
                epoch: Some("native-process-epoch".into()),
            },
            observed_from: "2026-11-15T12:00:00Z".into(),
            observed_until: "2026-11-15T12:00:01Z".into(),
            database_writable: Some(true),
        },
        activity: ActivityKey {
            session: NativeActivityIdentity { id: "42".into(), generation: "backend-start-1".into() },
            request: Some(NativeActivityIdentity { id: "42".into(), generation: "query-start-1".into() }),
            transaction: None,
        },
        statement: Some(NativeIdentity::new("native-statement-1")),
    }
}

#[test]
fn signal_target_round_trips_and_advancing_read_bounds_do_not_replace_a_session() {
    let original = target();
    let recovered: SessionActionTarget = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(original, recovered);
    let mut current = original.clone();
    current.context.observed_from = "2026-11-15T12:00:02Z".into();
    current.context.observed_until = "2026-11-15T12:00:03Z".into();
    for action in [SessionAction::Cancel, SessionAction::Terminate] {
        assert!(original.is_valid_for(action));
        assert!(original.matches_action(&current, action));
    }
}

#[test]
fn recycled_sessions_and_changed_native_authority_refuse_both_actions() {
    let original = target();
    for change in 0..6 {
        let mut current = original.clone();
        match change {
            0 => current.activity.session.generation = "replacement-backend".into(),
            1 => current.activity.session.id = "43".into(),
            2 => {
                current.context.target =
                    DatabaseIdentity::server(Engine::Postgres, "other", "fixture.invalid", 5432, None).into()
            }
            3 => current.context.database_writable = Some(false),
            4 => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut current.context.partition else {
                    panic!()
                };
                member.native = NativeIdentity::new("replacement-member");
            }
            _ => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut current.context.partition else { panic!() };
                *epoch = Some("restarted-process".into());
            }
        }
        for action in [SessionAction::Cancel, SessionAction::Terminate] {
            assert!(!original.matches_action(&current, action), "change {change}, {action:?}");
            assert_eq!(
                original.require_current(&current, action).unwrap_err().detail,
                Some(crate::ErrorDetail::SessionActionChanged)
            );
        }
    }
}

#[test]
fn cancel_keeps_the_original_request_while_termination_still_targets_the_same_session() {
    let original = target();
    for change in 0..4 {
        let mut current = original.clone();
        match change {
            0 => current.activity.request.as_mut().unwrap().generation = "next-query".into(),
            1 => current.statement = Some(NativeIdentity::new("next-statement")),
            2 => current.statement = None,
            _ => current.activity.request = None,
        }
        assert!(!original.matches_action(&current, SessionAction::Cancel), "change {change}");
        assert!(original.matches_action(&current, SessionAction::Terminate), "change {change}");
    }
    let mut no_request = original;
    no_request.activity.request = None;
    no_request.activity.transaction = Some("idle-transaction-generation".into());
    assert!(!no_request.is_valid_for(SessionAction::Cancel));
    assert!(no_request.is_valid_for(SessionAction::Terminate));
}

#[test]
fn logical_routes_and_clocks_without_native_ownership_never_authorize_a_signal() {
    let original = target();
    for change in 0..6 {
        let mut unknown = original.clone();
        match change {
            0 => unknown.context.partition = SourcePartition::Target,
            1 => {
                let SourcePartition::InstanceLocal { member, .. } = &mut unknown.context.partition else { panic!() };
                *member = None;
            }
            2 => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut unknown.context.partition else { panic!() };
                *epoch = None;
            }
            3 => unknown.activity.session.generation.clear(),
            4 => unknown.context.observed_until = "2026-11-15T11:59:59Z".into(),
            _ => unknown.statement = Some(NativeIdentity::new("")),
        }
        for action in [SessionAction::Cancel, SessionAction::Terminate] {
            assert!(!unknown.is_valid_for(action), "change {change}");
            assert!(!unknown.matches_action(&unknown, action));
        }
    }
}

#[test]
fn unknown_updateability_is_preserved_without_substituting_for_native_actor_identity() {
    let known = target();
    let mut unknown = known.clone();
    unknown.context.database_writable = None;
    for action in [SessionAction::Cancel, SessionAction::Terminate] {
        assert!(unknown.is_valid_for(action));
        assert!(unknown.matches_action(&unknown, action));
        assert!(!known.matches_action(&unknown, action));
        assert!(!unknown.matches_action(&known, action));
    }
    unknown.context.partition = SourcePartition::Target;
    assert!(!unknown.is_valid_for(SessionAction::Terminate));
}

#[test]
fn request_requires_an_original_named_route_and_signal_result_does_not_claim_completion() {
    let mut request = SessionActionRequest {
        route: SessionActionRoute { connection: "staging".into(), revision: crate::telemetry::digest(b"saved-route") },
        target: target(),
        action: SessionAction::Cancel,
    };
    request.validate().unwrap();
    request.route.revision = "a-connection-name-is-not-a-route-revision".into();
    assert!(request.validate().is_err());
    request.route.revision = crate::telemetry::digest(b"saved-route");
    request.route.connection.clear();
    assert!(request.validate().is_err());
    assert_eq!(serde_json::to_string(&SessionActionOutcome::SignalSent).unwrap(), "\"signal-sent\"");
}

#[test]
fn selecting_from_live_preserves_the_original_route_and_refuses_ambiguous_or_unqualified_rows() {
    let route = SessionActionRoute { connection: "staging".into(), revision: crate::telemetry::digest(b"route") };
    let row: Session = serde_json::from_value(serde_json::json!({
        "id": "42", "state": "active", "action_target": target()
    }))
    .unwrap();
    let original = LiveState {
        notes: vec![],
        taken_at: "2026-11-15T12:00:01Z".into(),
        sessions: vec![row.clone()],
        waits: vec![],
        session_actions: vec![SessionAction::Cancel, SessionAction::Terminate],
        session_actions_note: None,
        session_action_route: Some(route.clone()),
        blocking: None,
    };
    let request = original.action_request("42", SessionAction::Cancel).unwrap();
    assert_eq!(request.route, route);
    assert_eq!(request.target, row.action_target.clone().unwrap());
    assert!(original.action_request("43", SessionAction::Cancel).is_err());
    for change in 0..5 {
        let mut live = original.clone();
        match change {
            0 => live.sessions.push(row.clone()),
            1 => live.sessions[0].action_target = None,
            2 => live.session_action_route = None,
            3 => live.session_actions.clear(),
            _ => live.sessions[0].id = "another-actor".into(),
        }
        let selected = live.sessions[0].id.clone();
        assert!(live.action_request(&selected, SessionAction::Cancel).is_err(), "change {change}");
    }
}
