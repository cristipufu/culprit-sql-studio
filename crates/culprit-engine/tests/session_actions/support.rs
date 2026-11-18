use culprit_core::{
    DatabaseIdentity, Engine, LiveState, Session, SessionAction, SessionActionRequest, SessionActionRoute,
    SessionActionTarget,
    telemetry::{
        ActivityKey, CollectionTarget, NativeActivityIdentity, NativeIdentity, NativeReadContext,
        SharedResourceIdentity, SharedResourceKind, SourcePartition,
    },
};

/// Structurally valid synthetic native facts; this fixture supplies no production identity or cost authority.
pub(crate) fn target(engine: Engine, session: &str) -> SessionActionTarget {
    SessionActionTarget {
        context: NativeReadContext {
            target: CollectionTarget::Database(DatabaseIdentity::server(
                engine,
                "shop",
                "action.fixture.invalid",
                5432,
                Some("native-database-generation".into()),
            )),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("synthetic-native-namespace"),
                    native: NativeIdentity::new("synthetic-native-member"),
                }),
                epoch: Some("synthetic-native-process-generation".into()),
            },
            observed_from: "2026-11-15T10:00:00Z".into(),
            observed_until: "2026-11-15T10:00:01Z".into(),
            database_writable: Some(true),
        },
        activity: ActivityKey {
            session: NativeActivityIdentity { id: session.into(), generation: "2026-10-23T21:00:00.123456Z".into() },
            request: Some(NativeActivityIdentity {
                id: "native-request".into(),
                generation: "native-request-generation".into(),
            }),
            transaction: None,
        },
        statement: Some(NativeIdentity::new("native-original-statement")),
    }
}

pub(crate) fn live(engine: Engine, id: &str) -> LiveState {
    let target = target(engine, id);
    let displayed = engine.spec().live_session_id(&target.activity.session);
    let row: Session = serde_json::from_value(serde_json::json!({
        "id": displayed,
        "state": "active",
        "sql": "SELECT original_native_request",
        "action_target": target,
    }))
    .unwrap();
    LiveState {
        activity_census: None,
        session_census: None,
        notes: vec![],
        taken_at: "2026-11-15T10:00:01Z".into(),
        sessions: vec![row],
        waits: vec![],
        session_actions: vec![SessionAction::Cancel, SessionAction::Terminate],
        session_actions_note: None,
        session_action_route: None,
        blocking: None,
    }
}

/// Used only to prove refusal ordering; an arbitrary revision never supplies permission to perform an action.
pub(crate) fn unbound_request(connection: &str, action: SessionAction) -> SessionActionRequest {
    SessionActionRequest {
        route: SessionActionRoute { connection: connection.into(), revision: "0".repeat(64) },
        target: target(Engine::Postgres, "42"),
        action,
    }
}
