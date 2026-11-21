use super::*;

fn fixture_context(native: SessionNativeContext, idle: bool) -> (ActivityKey, ActivitySessionContext) {
    let postgres = matches!(native, SessionNativeContext::Postgres(_));
    let key = ActivityKey {
        session: NativeActivityIdentity {
            id: "42".into(),
            generation: if postgres { "2020-01-01T11:50:00.000001Z" } else { "1001" }.into(),
        },
        request: (!idle).then(|| NativeActivityIdentity {
            id: if postgres { "statement" } else { "3" }.into(),
            generation: if postgres { "2020-01-01T11:59:59.000001Z" } else { "4" }.into(),
        }),
        transaction: None,
    };
    (
        key,
        ActivitySessionContext {
            lifecycle: if idle { SessionLifecycle::Idle } else { SessionLifecycle::Running },
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: Some(Box::new(native)),
        },
    )
}
fn postgres() -> PostgresSessionContext {
    PostgresSessionContext {
        database_oid: Some("42".into()),
        database_name: Some("db".into()),
        backend_type: Some("client backend".into()),
    }
}
fn mysql() -> MysqlSessionContext {
    MysqlSessionContext {
        default_database: Some("session-default".into()),
        request_schema: Some("statement-default".into()),
        thread_type: Some("FOREGROUND".into()),
        thread_name: Some("thread/sql/one_connection".into()),
    }
}

#[test]
fn backend_database_and_utc_generation_roundtrip_without_unacquired_session_counters_or_library() {
    let (mut key, context) = fixture_context(SessionNativeContext::Postgres(postgres()), false);
    assert!(context.is_valid_for(&key, false));
    key.session.id = "native-node/42".into();
    assert!(context.is_valid_for(&key, false), "source-owned YSQL namespace does not become a borrowed physical PID");
    let restored: ActivitySessionContext = serde_json::from_slice(&serde_json::to_vec(&context).unwrap()).unwrap();
    assert_eq!(restored, context);
    let Some(SessionNativeContext::Postgres(native)) = restored.native.as_deref() else { panic!() };
    assert_eq!(native.database_oid.as_deref(), Some("42"));
    assert!(native.is_client_backend());
    assert!(restored.login_age_ms.is_none() && restored.client_library.is_none());
}

#[test]
fn native_null_zero_and_worker_backend_kind_remain_raw_without_establishing_client_database_identity() {
    for oid in [None, Some("0"), Some("4294967295")] {
        let mut native = postgres();
        native.database_oid = oid.map(str::to_owned);
        let (key, context) = fixture_context(SessionNativeContext::Postgres(native), true);
        assert!(context.is_valid_for(&key, true));
        assert_eq!(
            serde_json::from_slice::<ActivitySessionContext>(&serde_json::to_vec(&context).unwrap()).unwrap(),
            context
        );
    }
    for kind in [
        None,
        Some("parallel worker"),
        Some("autovacuum worker"),
        Some("yb-conn-mgr worker connection"),
        Some("future native kind"),
    ] {
        let mut native = postgres();
        native.backend_type = kind.map(str::to_owned);
        assert!(!native.is_client_backend());
        let (key, context) = fixture_context(SessionNativeContext::Postgres(native), false);
        assert!(context.is_valid_for(&key, false));
    }
}

#[test]
fn malformed_backend_oid_or_unzoned_generation_refuses_binding_without_overriding_generic_metadata() {
    for invalid in ["-1", "042", "42.0", "4294967296"] {
        let mut native = postgres();
        native.database_oid = Some(invalid.into());
        let (key, context) = fixture_context(SessionNativeContext::Postgres(native), false);
        assert!(!context.is_valid_for(&key, false), "{invalid}");
    }
    let (mut key, mut context) = fixture_context(SessionNativeContext::Postgres(postgres()), false);
    key.session.generation = "2020-01-01T11:50:00".into();
    assert!(!context.is_valid_for(&key, false));
    key.session.generation = "2020-01-01T11:50:00Z".into();
    context.login_age_ms = Some("1".into());
    assert!(
        context.is_valid_for(&key, false),
        "a separately recorded generic clock age is not the native elapsed counter"
    );
}

#[test]
fn mutable_thread_default_and_bound_statement_schema_remain_distinct_with_unknown_connection_age() {
    let (key, context) = fixture_context(SessionNativeContext::Mysql(mysql()), false);
    assert!(context.is_valid_for(&key, false));
    let restored: ActivitySessionContext = serde_json::from_slice(&serde_json::to_vec(&context).unwrap()).unwrap();
    let Some(SessionNativeContext::Mysql(native)) = restored.native.as_deref() else { panic!() };
    assert_eq!(native.default_database.as_deref(), Some("session-default"));
    assert_eq!(native.request_schema.as_deref(), Some("statement-default"));
    assert!(native.is_client_thread(Some("Query")));
    assert!(restored.login_age_ms.is_none());
    let mut missing = serde_json::to_value(&context).unwrap();
    missing["native"]["data"].as_object_mut().unwrap().remove("default_database");
    assert!(serde_json::from_value::<ActivitySessionContext>(missing).is_err());
}

#[test]
fn native_thread_class_and_type_are_required_for_client_eligibility_independently_of_labels() {
    for (kind, name, command, expected) in [
        (Some("FOREGROUND"), Some("thread/sql/one_connection"), Some("Query"), true),
        (Some("FOREGROUND"), Some("thread/sql/one_connection"), Some("Sleep"), true),
        (Some("BACKGROUND"), Some("thread/sql/one_connection"), Some("Query"), false),
        (Some("FOREGROUND"), Some("thread/sql/slave_sql"), Some("Query"), false),
        (Some("FOREGROUND"), None, Some("Query"), false),
        (None, Some("thread/sql/one_connection"), Some("Query"), false),
        (Some("FOREGROUND"), Some("thread/sql/one_connection"), Some("Binlog Dump"), false),
        (Some("FOREGROUND"), Some("thread/sql/one_connection"), Some("future command"), false),
    ] {
        let mut native = mysql();
        native.thread_type = kind.map(str::to_owned);
        native.thread_name = name.map(str::to_owned);
        assert_eq!(native.is_client_thread(command), expected, "{kind:?}/{name:?}/{command:?}");
    }
}

#[test]
fn missing_default_never_borrows_statement_schema_and_idle_rows_cannot_retain_previous_event_as_current() {
    let mut native = mysql();
    native.default_database = None;
    let (key, context) = fixture_context(SessionNativeContext::Mysql(native), false);
    assert!(context.is_valid_for(&key, false));
    let (mut key, mut context) = fixture_context(SessionNativeContext::Mysql(mysql()), false);
    key.session.generation = "0".into();
    assert!(!context.is_valid_for(&key, false));
    key.session.generation = "1001".into();
    key.request = None;
    context.lifecycle = SessionLifecycle::Idle;
    assert!(!context.is_valid_for(&key, true), "a previous current-schema value is not an idle current request");
}
