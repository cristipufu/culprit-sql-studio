use super::*;
mod session_authority;

pub(crate) fn native_session_context(row: &mut ActivityObservation, state: &str, login: &str) {
    row.key.session.generation = login.into();
    let lifecycle = MssqlSessionContext::lifecycle_from_native_state(state);
    let login_established = MssqlSessionContext::login_is_established(login, lifecycle);
    row.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle,
        login_age_ms: login_established.then(|| "0".into()),
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: Some(Box::new(SessionNativeContext::Mssql(MssqlSessionContext {
            native_state: state.into(),
            login_established,
            tds_protocol_version: None,
            elapsed_ms: None,
            session_database_id: None,
            session_database_name: None,
            request_database_id: None,
            times: MssqlSessionTimeTokens {
                login_without_offset: login.into(),
                last_request_start_without_offset: None,
                last_request_end_without_offset: None,
            },
            has_current_request: !row.idle,
            request_connection_id: None,
            idle_connection: None,
        }))),
    }));
}

#[test]
fn request_page_server_counts_keep_exact_native_bigint_and_do_not_compare_missing_or_decreased_counters() {
    let mut row = request("1");
    for count in ["0", "9007199254740993", "9223372036854775807"] {
        row.request_counters.as_mut().unwrap().page_server_reads = Some(count.into());
        assert!(row.is_valid());
        let encoded = serde_json::to_string(&row).unwrap();
        let decoded: ActivityObservation = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.request_counters.as_ref().unwrap().page_server_reads.as_deref(), Some(count));
        assert!(decoded.session_counters.page_server_reads.is_none());
    }
    for invalid in ["-1", "00", "1.0", "9223372036854775808", "18446744073709551615"] {
        row.request_counters.as_mut().unwrap().page_server_reads = Some(invalid.into());
        assert!(!row.is_valid(), "{invalid}");
    }
    let count = |value: &str| ActivityCounters { page_server_reads: Some(value.into()), ..Default::default() };
    assert_eq!(count("9007199254740994").progress(&count("9007199254740993")), ActivityProgress::ChangedCounters);
    assert_eq!(count("9007199254740993").progress(&count("9007199254740994")), ActivityProgress::Unknown);
    assert_eq!(count("0").progress(&ActivityCounters::default()), ActivityProgress::Unknown);
    assert_eq!(count("0").progress(&count("0")), ActivityProgress::NoObservedCounterChange);
}

pub(crate) fn request(id: &str) -> ActivityObservation {
    ActivityObservation {
        session_context: None,
        blocking_status: crate::telemetry::ActivityBlockingStatus::Known,
        waiting_on_lock: None,
        key: ActivityKey {
            session: NativeActivityIdentity { id: id.into(), generation: "login-1".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-1".into() }),
            transaction: None,
        },
        idle: false,
        scope: ActivityScopeBasis::RequestDatabase,
        state: "running".into(),
        database: Some("sample".into()),
        command: Some("SELECT".into()),
        last_request_age_ms: None,
        open_transactions: Some(0),
        elapsed_ms: Some("100000".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters::default(),
        request_counters: Some(ActivityCounters {
            cpu_ms: Some("0".into()),
            logical_reads: Some("0".into()),
            ..ActivityCounters::default()
        }),
        wait: None,
        query_hash: None,
        statement_identity: None,
        client: None,
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    }
}
fn idle(id: &str) -> ActivityObservation {
    let mut row = request(id);
    row.key.request = None;
    row.key.transaction = Some("transaction-1".into());
    row.idle = true;
    row.scope = ActivityScopeBasis::TransactionDatabase;
    row.state = "sleeping".into();
    row.open_transactions = Some(1);
    row.request_counters = None;
    row.elapsed_ms = None;
    row.transaction_age_ms = Some("300000".into());
    row
}
fn blocked(row: &mut ActivityObservation, owners: Vec<ActivityWaitOwner>) {
    row.elapsed_ms = Some("1000".into());
    row.wait = Some(ActivityWait {
        class: None,
        identified_resources: vec![],
        resources: None,
        native_type: Some("LCK_M_X".into()),
        elapsed_ms: Some("40000".into()),
        owners,
    });
}
fn owner(id: &str) -> ActivityWaitOwner {
    ActivityWaitOwner::Session { id: id.into(), generation: Some("login-1".into()) }
}

#[test]
fn healthy_sessions_are_not_retained_and_idle_transactions_need_real_blocking_evidence() {
    let mut healthy = request("1");
    healthy.elapsed_ms = Some("100".into());
    let evidence = ActivityEvidence::select(&[healthy, idle("2")], &ActivityPolicy::default()).unwrap();
    assert!(evidence.observations.is_empty());
    assert!(evidence.conditions.is_empty());
    assert!(evidence.blocking.is_empty());
}
#[test]
fn multiple_owners_and_a_cycle_retain_bounded_context_without_recursive_growth() {
    let mut waiter = request("1");
    blocked(&mut waiter, vec![owner("2"), owner("3")]);
    let mut middle = request("2");
    blocked(&mut middle, vec![owner("1")]);
    let evidence = ActivityEvidence::select(&[waiter, middle, idle("3")], &ActivityPolicy::default()).unwrap();
    assert_eq!(evidence.observations.len(), 3);
    assert_eq!(evidence.blocking.len(), 3);
    assert!(evidence.blocking.iter().all(|edge| edge.owner_observed));
    assert!(evidence.conditions.values().any(|conditions| conditions == &[ActivityCondition::IdleBlocker]));
}
#[test]
fn untracked_wait_owners_and_unobserved_sessions_are_not_invented_blocker_nodes() {
    let mut waiter = request("1");
    blocked(
        &mut waiter,
        vec![ActivityWaitOwner::Unknown { native_id: "-5".into(), reason: UnknownWaitOwner::UntrackedLatch }],
    );
    assert!(ActivityEvidence::select(&[waiter.clone()], &ActivityPolicy::default()).unwrap().observations.is_empty());
    blocked(&mut waiter, vec![owner("missing")]);
    let evidence = ActivityEvidence::select(&[waiter], &ActivityPolicy::default()).unwrap();
    assert_eq!(evidence.observations.len(), 1);
    assert_eq!(evidence.blocking.len(), 1);
    assert!(!evidence.blocking[0].owner_observed);
}
#[test]
fn reused_sessions_inside_a_read_and_duplicate_request_identities_invalidate_the_observation() {
    let first = request("1");
    let mut reused = first.clone();
    reused.key.session.generation = "login-2".into();
    assert!(ActivityEvidence::select(&[first.clone(), reused], &ActivityPolicy::default()).is_err());
    assert!(ActivityEvidence::select(&[first.clone(), first.clone()], &ActivityPolicy::default()).is_err());
    let mut parallel = first.clone();
    parallel.key.request.as_mut().unwrap().id = "1".into();
    assert_eq!(ActivityEvidence::select(&[first, parallel], &ActivityPolicy::default()).unwrap().observations.len(), 2);
}
#[test]
fn unknown_counters_remain_unknown_and_resets_are_not_progress() {
    let zero = ActivityCounters { cpu_ms: Some("0".into()), ..ActivityCounters::default() };
    assert_eq!(ActivityCounters::default().progress(&ActivityCounters::default()), ActivityProgress::Unknown);
    assert_eq!(zero.progress(&zero), ActivityProgress::NoObservedCounterChange);
    let padded_zero = ActivityCounters { cpu_ms: Some("00".into()), ..ActivityCounters::default() };
    assert_eq!(zero.progress(&padded_zero), ActivityProgress::NoObservedCounterChange);
    let high = ActivityCounters { cpu_ms: Some("9007199254740993".into()), ..ActivityCounters::default() };
    assert_eq!(high.progress(&zero), ActivityProgress::ChangedCounters);
    assert_eq!(zero.progress(&high), ActivityProgress::Unknown);
}

#[test]
fn native_lock_wait_without_visible_owner_needs_measured_wait_duration_for_threshold_blocking() {
    let mut row = request("1");
    row.elapsed_ms = Some("40000".into());
    row.blocking_status = ActivityBlockingStatus::Unknown;
    row.waiting_on_lock = Some(true);
    row.wait = Some(ActivityWait {
        class: None,
        identified_resources: vec![],
        native_type: Some("native-lock-wait".into()),
        elapsed_ms: None,
        owners: vec![],
        resources: None,
    });
    assert!(row.is_valid());
    assert!(row.conditions(&ActivityPolicy::default()).is_empty());
    row.wait.as_mut().unwrap().elapsed_ms = Some("29999".into());
    assert!(row.conditions(&ActivityPolicy::default()).is_empty());
    row.wait.as_mut().unwrap().elapsed_ms = Some("30000".into());
    assert_eq!(row.conditions(&ActivityPolicy::default()), [ActivityCondition::Blocked]);
    assert!(ActivityEvidence::select(&[row], &ActivityPolicy::default()).unwrap().blocking.is_empty());
}

#[test]
fn request_age_never_supplies_wait_duration_and_unknown_lock_measurements_stay_uncertain() {
    let mut row = request("1");
    row.elapsed_ms = Some("40000".into());
    row.blocking_status = ActivityBlockingStatus::Unknown;
    row.wait = Some(ActivityWait {
        class: None,
        identified_resources: vec![],
        native_type: Some("native-wait".into()),
        elapsed_ms: None,
        owners: vec![owner("missing")],
        resources: None,
    });
    let policy = ActivityPolicy::default();
    assert!(row.conditions(&policy).is_empty());
    assert!(row.blocking_uncertain(&policy));
    row.wait.as_mut().unwrap().elapsed_ms = Some("1".into());
    assert!(row.conditions(&policy).is_empty());
    row.wait.as_mut().unwrap().elapsed_ms = Some("30000".into());
    assert_eq!(row.conditions(&policy), [ActivityCondition::Blocked]);
    row.wait.as_mut().unwrap().owners.clear();
    assert!(row.conditions(&policy).is_empty(), "an unidentified long native wait is not a lock verdict");
}

#[test]
fn current_lock_wait_unknown_and_native_negative_are_distinct_wire_facts() {
    let mut row = request("1");
    for verdict in [None, Some(false), Some(true)] {
        row.waiting_on_lock = verdict;
        let value = serde_json::to_value(&row).unwrap();
        assert_eq!(value["waiting_on_lock"], serde_json::to_value(verdict).unwrap());
        assert_eq!(serde_json::from_value::<ActivityObservation>(value).unwrap(), row);
    }
}
