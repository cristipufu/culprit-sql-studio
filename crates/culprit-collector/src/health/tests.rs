use super::*;
pub(super) fn health() -> CollectorHealth {
    CollectorHealth::new(
        &FleetInventory {
            targets: vec![FleetTarget {
                id: "private-connection-canary".into(),
                subject: culprit_core::telemetry::CollectionSubject::Database,
                connection: "private-connection-canary".into(),
                shard: "alpha".into(),
                admission_group: "pool".into(),
                schedules: vec![FleetSchedule {
                    id: "private-schedule-canary".into(),
                    every_seconds: 60,
                    jitter_seconds: 0,
                    collection: ScheduledCollection::Activity { policy: ActivityPolicy::default() },
                    budget: CollectionBudget::default(),
                }],
            }],
        },
        "alpha",
    )
    .unwrap()
}
pub(super) fn status(debts: u32) -> OutboxStatus {
    OutboxStatus {
        outbox_id: "fixture".into(),
        limits: OutboxLimits::default(),
        captures: 0,
        ready: 0,
        source_debts: debts,
        unconfirmed_sources: debts,
        unfinished: 0,
        charged_bytes: 0,
        entries: vec![],
        next_cursor: None,
    }
}
fn event(status: CollectionStatus) -> FleetCollectionEvent {
    FleetCollectionEvent {
        target: "private-connection-canary".into(),
        connection: "private-connection-canary".into(),
        schedule: "private-schedule-canary".into(),
        work: FleetWork::Capture,
        status,
        capture: None,
        error_code: None,
        gap: None,
    }
}

#[test]
fn database_and_pool_jobs_on_one_connection_keep_independent_health() {
    let schedule = FleetSchedule {
        id: "health".into(),
        every_seconds: 60,
        jitter_seconds: 0,
        collection: ScheduledCollection::Snapshot { dataset: culprit_core::telemetry::SnapshotDataset::Operational },
        budget: CollectionBudget::default(),
    };
    let database = FleetTarget {
        id: "database-owner".into(),
        connection: "member".into(),
        subject: culprit_core::telemetry::CollectionSubject::Database,
        shard: "alpha".into(),
        admission_group: "pool".into(),
        schedules: vec![schedule],
    };
    let mut pool = database.clone();
    pool.id = "pool-owner".into();
    pool.subject = culprit_core::telemetry::CollectionSubject::Shared {
        resource: culprit_core::telemetry::SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: culprit_core::telemetry::SharedResourceKind::Pool,
            namespace: culprit_core::telemetry::NativeIdentity::new("fixture-server"),
            native: culprit_core::telemetry::NativeIdentity::new("fixture-pool"),
        },
    };
    let health = CollectorHealth::new(&FleetInventory { targets: vec![database, pool] }, "alpha").unwrap();
    let mut event = FleetCollectionEvent {
        target: "pool-owner".into(),
        connection: "unrelated-route".into(),
        schedule: "health".into(),
        work: FleetWork::Capture,
        status: CollectionStatus::Complete,
        capture: None,
        error_code: None,
        gap: None,
    };
    health.collection(&event).unwrap();
    health.with(|state| assert!(state.schedules.values().all(|schedule| schedule.attempts == 0))).unwrap();
    event.connection = "member".into();
    health.collection(&event).unwrap();
    health
        .with(|state| {
            assert_eq!(state.schedules.len(), 2);
            assert_eq!(state.schedules[&("database-owner".into(), "health".into())].attempts, 0);
            assert_eq!(state.schedules[&("pool-owner".into(), "health".into())].complete, 1);
        })
        .unwrap();
}
#[test]
fn missing_attempts_unknown_completion_and_overdue_expected_work_do_not_claim_successful_collection() {
    let health = health();
    assert!(!health.ready().unwrap());
    health.initialized().unwrap();
    health.outbox(&status(0)).unwrap();
    assert!(health.ready().unwrap());
    let mut active = status(0);
    active.source_debts = 1;
    health.outbox(&active).unwrap();
    assert!(health.ready().unwrap(), "an owned active read is distinct from an unconfirmed shutdown");
    let body = String::from_utf8(health.reply().unwrap().metrics).unwrap();
    assert!(!body.contains("private-connection-canary"));
    assert!(!body.contains("private-schedule-canary"));
    assert!(!body.contains("last_completed_operation_age_seconds"));
    health.with(|s| s.started = Instant::now() - Duration::from_secs(3600)).unwrap();
    assert!(!health.ready().unwrap());
    assert!(
        String::from_utf8(health.reply().unwrap().metrics)
            .unwrap()
            .lines()
            .any(|s| s.starts_with("culprit_collector_schedule_overdue{") && s.ends_with(" 1"))
    );
}
#[test]
fn an_operation_failure_or_native_debt_blocks_readiness_until_a_later_success_without_hiding_delivery_errors() {
    let health = health();
    health.initialized().unwrap();
    health.outbox(&status(0)).unwrap();
    let mut event = event(CollectionStatus::Incomplete(CollectionStop::SourceUnavailable));
    health.collection(&event).unwrap();
    assert!(!health.ready().unwrap());
    event.status = CollectionStatus::Complete;
    health.collection(&event).unwrap();
    assert!(health.ready().unwrap());
    health.outbox(&status(1)).unwrap();
    assert!(!health.ready().unwrap());
    health.delivery(&Err(DeliveryProblem::Transport.error())).unwrap();
    health.outbox(&status(0)).unwrap();
    assert!(
        health.ready().unwrap(),
        "a destination outage does not claim that the process is dead or SQL collection failed"
    );
    let body = String::from_utf8(health.reply().unwrap().metrics).unwrap();
    assert!(body.contains("culprit_collector_delivery_errors_total{problem=\"transport\"} 1"));
    assert!(body.contains("last_completed_operation_age_seconds"));
}

#[test]
fn expected_source_waits_have_bounded_startup_grace_and_never_count_as_completed_work() {
    let health = health();
    health.initialized().unwrap();
    health.outbox(&status(0)).unwrap();
    let waiting = event(CollectionStatus::Incomplete(CollectionStop::SourceNotReady));
    health.collection(&waiting).unwrap();
    assert!(health.ready().unwrap());
    health
        .with(|state| {
            let schedule = state.schedules.values().next().unwrap();
            assert_eq!(schedule.complete, 0);
            assert_eq!(schedule.incomplete, 1);
            assert!(schedule.last_complete.is_none());
            assert!(!schedule.last_failed);
            state.started = Instant::now() - Duration::from_secs(3600);
        })
        .unwrap();
    health.collection(&waiting).unwrap();
    assert!(!health.ready().unwrap(), "fresh metadata attempts cannot extend startup grace");
    let metrics = String::from_utf8(health.reply().unwrap().metrics).unwrap();
    assert!(
        metrics.lines().any(|line| line.starts_with("culprit_collector_schedule_overdue{") && line.ends_with(" 1"))
    );
    health.collection(&event(CollectionStatus::Complete)).unwrap();
    assert!(health.ready().unwrap());
    health
        .with(|state| {
            let schedule = state.schedules.values().next().unwrap();
            assert_eq!(schedule.complete, 1);
            assert_eq!(schedule.incomplete, 2);
        })
        .unwrap();
}

#[test]
fn expected_waits_preserve_prior_errors_and_cannot_hide_stale_completed_work() {
    let health = health();
    health.initialized().unwrap();
    health.outbox(&status(0)).unwrap();
    health.collection(&event(CollectionStatus::Complete)).unwrap();
    health.collection(&event(CollectionStatus::Incomplete(CollectionStop::SourceUnavailable))).unwrap();
    let waiting = event(CollectionStatus::Incomplete(CollectionStop::SourceNotReady));
    health.collection(&waiting).unwrap();
    assert!(!health.ready().unwrap(), "a wait is not proof that the previous error recovered");
    health
        .with(|state| {
            let schedule = state.schedules.values().next().unwrap();
            assert!(schedule.last_failed);
            assert_eq!(schedule.complete, 1);
            assert_eq!(schedule.incomplete, 2);
        })
        .unwrap();
    health.collection(&event(CollectionStatus::Complete)).unwrap();
    assert!(health.ready().unwrap());
    health
        .with(|state| {
            let schedule = state.schedules.values_mut().next().unwrap();
            schedule.last_complete = Some(Instant::now() - Duration::from_secs(3600));
        })
        .unwrap();
    health.collection(&waiting).unwrap();
    assert!(!health.ready().unwrap(), "fresh attempts cannot refresh stale completion evidence");
    health.collection(&event(CollectionStatus::Complete)).unwrap();
    assert!(health.ready().unwrap());
}

#[test]
fn delivery_only_disables_source_expectations_without_hiding_native_debt_or_fencing() {
    let health = health();
    health.initialized().unwrap();
    health.outbox(&status(0)).unwrap();
    health.with(|s| s.started = Instant::now() - Duration::from_secs(3600)).unwrap();
    assert!(!health.ready().unwrap());
    health.source_disabled().unwrap();
    assert!(health.ready().unwrap());
    let body = String::from_utf8(health.reply().unwrap().metrics).unwrap();
    assert!(body.contains("culprit_collector_source_enabled 0"));
    assert!(body.lines().any(|line| line.starts_with("culprit_collector_schedule_overdue{") && line.ends_with(" 0")));
    health.outbox(&status(1)).unwrap();
    assert!(!health.ready().unwrap());
    health.outbox(&status(0)).unwrap();
    health.source_paused().unwrap();
    assert!(!health.ready().unwrap());
}
