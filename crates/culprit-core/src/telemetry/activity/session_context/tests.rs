use super::*;

fn session_context_fixture(request: bool) -> (ActivityKey, ActivitySessionContext) {
    let key = ActivityKey {
        session: NativeActivityIdentity { id: "42".into(), generation: "2020-01-01T07:00:00.1533333".into() },
        request: request
            .then(|| NativeActivityIdentity { id: "1".into(), generation: "2020-01-01T07:58:00.3066667".into() }),
        transaction: None,
    };
    let context = ActivitySessionContext {
        lifecycle: if request { SessionLifecycle::Running } else { SessionLifecycle::Idle },
        login_age_ms: Some("0".into()),
        last_request_end_age_ms: None,
        client_library: Some("native-driver-context".into()),
        client_process_id: Some("0".into()),
        native: Some(Box::new(SessionNativeContext::Mssql(MssqlSessionContext {
            native_state: if request { "Running" } else { "Sleeping" }.into(),
            login_established: true,
            tds_protocol_version: None,
            elapsed_ms: None,
            session_database_id: None,
            session_database_name: None,
            request_database_id: None,
            times: MssqlSessionTimeTokens {
                login_without_offset: key.session.generation.clone(),
                last_request_start_without_offset: Some("2020-01-01T07:59:00.0000000".into()),
                last_request_end_without_offset: Some("2020-01-01T07:55:00.0000000".into()),
            },
            has_current_request: request,
            request_connection_id: request.then(|| "a8a44410-5033-4db1-9e90-79354ab31702".into()),
        }))),
    };
    (key, context)
}

#[test]
fn serialization_preserves_zero_unknown_and_independent_native_request_clock_tokens() {
    let (key, context) = session_context_fixture(true);
    assert!(context.is_valid_for(&key, false));
    let restored: ActivitySessionContext = serde_json::from_slice(&serde_json::to_vec(&context).unwrap()).unwrap();
    assert_eq!(restored, context);
    assert_eq!(restored.login_age_ms.as_deref(), Some("0"));
    assert!(restored.last_request_end_age_ms.is_none());
    let SessionNativeContext::Mssql(native) = restored.native.as_deref().unwrap();
    assert!(native.tds_protocol_version.is_none());
    assert_eq!(native.times.last_request_end_without_offset.as_deref(), Some("2020-01-01T07:55:00.0000000"));
}

#[test]
fn native_context_cannot_borrow_another_login_request_connection_or_a_fabricated_offset() {
    for change in ["login", "connection", "offset", "precision", "negative"] {
        let (key, mut context) = session_context_fixture(true);
        let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
        match change {
            "login" => native.times.login_without_offset = "2020-01-01T07:01:00.0000000".into(),
            "connection" => native.request_connection_id = Some("not-a-native-connection-token".into()),
            "offset" => native.times.last_request_start_without_offset = Some("2020-01-01T07:59:00Z".into()),
            "precision" => {
                native.times.last_request_start_without_offset = Some("2020-01-01T07:59:00.1234567891".into())
            }
            _ => context.login_age_ms = Some("-1".into()),
        }
        assert!(!context.is_valid_for(&key, false), "{change}");
    }
}

#[test]
fn idle_context_cannot_inherit_a_current_request_connection_or_established_prelogin_authority() {
    let (key, mut context) = session_context_fixture(false);
    assert!(context.is_valid_for(&key, true));
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.request_connection_id = Some("a8a44410-5033-4db1-9e90-79354ab31702".into());
    assert!(!context.is_valid_for(&key, true));
    let (key, mut context) = session_context_fixture(true);
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.login_established = false;
    assert!(!context.is_valid_for(&key, false));
}

#[test]
fn the_native_prelogin_sentinel_is_retained_without_a_login_age_or_established_generation_claim() {
    let (mut key, mut context) = session_context_fixture(false);
    key.session.generation = "1900-01-01T00:00:00.0000000".into();
    context.lifecycle = SessionLifecycle::PoolResetPrelogin;
    context.login_age_ms = None;
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.times.login_without_offset = key.session.generation.clone();
    native.native_state = "Dormant".into();
    native.login_established = false;
    assert!(context.is_valid_for(&key, true));
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.login_established = true;
    assert!(!context.is_valid_for(&key, true));
}

#[test]
fn native_protocol_and_client_process_codes_keep_signed_int_representation_without_changing_unsigned_work_counters() {
    let (key, mut context) = session_context_fixture(false);
    context.client_process_id = Some(i32::MIN.to_string());
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.tds_protocol_version = Some(i32::MIN.to_string());
    assert!(context.is_valid_for(&key, true));
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.tds_protocol_version = Some((i32::MAX as i64 + 1).to_string());
    assert!(!context.is_valid_for(&key, true));
}

#[test]
fn canonical_native_session_state_and_declared_lifecycle_cannot_disagree_or_be_repaired_during_reanalysis() {
    let (key, mut context) = session_context_fixture(false);
    let original = serde_json::to_value(&context).unwrap();
    let mut contradictory = original.clone();
    contradictory["native"]["data"]["native_state"] = serde_json::json!("Running");
    let restored: ActivitySessionContext = serde_json::from_value(contradictory).unwrap();
    assert!(!restored.is_valid_for(&key, true));
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.native_state = "future-native-status".into();
    context.lifecycle = SessionLifecycle::Unknown;
    assert!(context.is_valid_for(&key, true));
    for (native, expected) in [
        ("Running", SessionLifecycle::Running),
        ("SLEEPING", SessionLifecycle::Idle),
        ("Dormant", SessionLifecycle::PoolResetPrelogin),
        ("Preconnect", SessionLifecycle::Connecting),
    ] {
        assert_eq!(MssqlSessionContext::lifecycle_from_native_state(native), expected);
    }
    assert_eq!(original["native"]["data"]["native_state"], "Sleeping");
}

#[test]
fn native_session_and_request_database_ids_roundtrip_independently_without_a_name_or_statement_binding() {
    let (key, mut context) = session_context_fixture(true);
    let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
    native.session_database_id = Some("4".into());
    native.session_database_name = Some("session-default".into());
    native.request_database_id = Some("7".into());
    assert!(context.is_valid_for(&key, false));
    let restored: ActivitySessionContext = serde_json::from_slice(&serde_json::to_vec(&context).unwrap()).unwrap();
    assert_eq!(restored, context);
    let SessionNativeContext::Mssql(native) = restored.native.as_deref().unwrap();
    assert_eq!(native.session_database_id.as_deref(), Some("4"));
    assert_eq!(native.session_database_name.as_deref(), Some("session-default"));
    assert_eq!(native.request_database_id.as_deref(), Some("7"));
}

#[test]
fn unknown_and_native_zero_database_ids_stay_distinct_without_target_name_fallback() {
    for native_id in [None, Some("0"), Some("32767")] {
        let (key, mut context) = session_context_fixture(false);
        let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
        native.session_database_id = native_id.map(str::to_owned);
        assert!(context.is_valid_for(&key, true));
        let restored: ActivitySessionContext = serde_json::from_slice(&serde_json::to_vec(&context).unwrap()).unwrap();
        assert_eq!(restored, context);
        let SessionNativeContext::Mssql(native) = restored.native.as_deref().unwrap();
        assert!(native.session_database_name.is_none());
        assert!(native.request_database_id.is_none());
    }
}

#[test]
fn malformed_native_database_ids_or_idle_request_metadata_cannot_gain_a_database_association() {
    for change in
        ["negative", "padded", "too-wide", "fraction", "name-without-id", "name-at-zero", "wide-name", "idle-request"]
    {
        let (key, mut context) = session_context_fixture(false);
        let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
        match change {
            "negative" => native.session_database_id = Some("-1".into()),
            "padded" => native.session_database_id = Some("07".into()),
            "too-wide" => native.session_database_id = Some("32768".into()),
            "fraction" => native.session_database_id = Some("7.0".into()),
            "name-without-id" => native.session_database_name = Some("reader-target".into()),
            "name-at-zero" => {
                native.session_database_id = Some("0".into());
                native.session_database_name = Some("reader-target".into());
            }
            "wide-name" => {
                native.session_database_id = Some("7".into());
                native.session_database_name = Some("x".repeat(513));
            }
            _ => native.request_database_id = Some("7".into()),
        }
        assert!(!context.is_valid_for(&key, true), "{change}");
    }
}

#[test]
fn native_session_elapsed_is_required_nullable_exact_int_and_independent_of_derived_login_age() {
    for elapsed in [None, Some("0"), Some("123456"), Some("2147483647"), Some("-1"), Some("-2147483648")] {
        let (key, mut context) = session_context_fixture(false);
        let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
        native.elapsed_ms = elapsed.map(str::to_owned);
        assert!(context.is_valid_for(&key, true));
        let restored: ActivitySessionContext = serde_json::from_slice(&serde_json::to_vec(&context).unwrap()).unwrap();
        assert_eq!(restored, context);
        assert_eq!(restored.login_age_ms.as_deref(), Some("0"));
        let SessionNativeContext::Mssql(native) = restored.native.as_deref().unwrap();
        assert_eq!(native.elapsed_ms.as_deref(), elapsed);
    }
    let (_, context) = session_context_fixture(false);
    let mut missing = serde_json::to_value(&context).unwrap();
    missing["native"]["data"].as_object_mut().unwrap().remove("elapsed_ms");
    assert!(serde_json::from_value::<ActivitySessionContext>(missing).is_err());
}

#[test]
fn malformed_or_out_of_native_range_elapsed_cannot_become_a_groupable_session_fact() {
    for elapsed in ["01", "1.0", "2147483648", "-2147483649", "18446744073709551615", "unknown"] {
        let (key, mut context) = session_context_fixture(false);
        let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap();
        native.elapsed_ms = Some(elapsed.into());
        assert!(!context.is_valid_for(&key, true), "{elapsed}");
    }
}
