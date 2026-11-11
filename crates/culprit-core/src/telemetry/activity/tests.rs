use super::*;

pub(crate) fn request(id: &str) -> ActivityObservation {
    ActivityObservation {
        blocking_status: crate::telemetry::ActivityBlockingStatus::Known,
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
        open_transactions: 0,
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
    row.open_transactions = 1;
    row.request_counters = None;
    row.elapsed_ms = None;
    row.transaction_age_ms = Some("300000".into());
    row
}
fn blocked(row: &mut ActivityObservation, owners: Vec<ActivityWaitOwner>) {
    row.elapsed_ms = Some("1000".into());
    row.wait = Some(ActivityWait {
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
