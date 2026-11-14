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
        native_coverage: None,
        capture: None,
        error_code: None,
        gap: None,
    }
}

#[test]
fn the_largest_supported_schedule_population_can_report_all_native_observations() {
    let health = health();
    let schedules = MAX_FLEET_TARGETS * MAX_TARGET_SCHEDULES;
    health
        .with(|state| {
            let mut template = state.schedules.values().next().unwrap().clone();
            let observed = Instant::now();
            template.last_attempt = Some(observed);
            template.last_complete = Some(observed);
            template.last_native_observation = Some(observed);
            template.native_coverage = Some(SourceState::Unavailable);
            template.attempts = u64::MAX;
            template.complete = u64::MAX;
            template.incomplete = u64::MAX;
            state.schedules.clear();
            for ordinal in 0..schedules {
                let mut schedule = template.clone();
                schedule.id = digest(&ordinal.to_le_bytes());
                state.schedules.insert((ordinal.to_string(), "work".into()), schedule);
            }
        })
        .unwrap();
    let reply = health.reply().unwrap();
    assert!(reply.metrics.len() < MAX_METRICS_BYTES);
    let metrics = String::from_utf8(reply.metrics).unwrap();
    assert_eq!(metrics.matches("culprit_collector_schedule_native_coverage{").count(), schedules);
    assert!(!metrics.contains("private-connection-canary"));
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
        native_coverage: None,
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

#[test]
fn successful_capture_operations_do_not_claim_unavailable_native_sources_are_healthy() {
    let health = health();
    health.initialized().unwrap();
    health.outbox(&status(0)).unwrap();
    let mut captured = event(CollectionStatus::Complete);
    captured.native_coverage = Some(SourceState::Unavailable);
    health.collection(&captured).unwrap();
    assert!(health.ready().unwrap(), "the process can keep running and report a native coverage gap");
    let metrics = String::from_utf8(health.reply().unwrap().metrics).unwrap();
    assert!(
        metrics
            .lines()
            .any(|line| line.starts_with("culprit_collector_schedule_completed_operations_total{")
                && line.ends_with(" 1"))
    );
    assert!(metrics.lines().any(|line| line.starts_with("culprit_collector_schedule_native_coverage{")
        && line.contains("state=\"unavailable\"")));
    assert!(!metrics.contains("state=\"collected\""));
    assert!(!metrics.contains("private-connection-canary"));
}

#[test]
fn metadata_operations_cannot_refresh_or_replace_native_population_qualification() {
    let health = health();
    let mut captured = event(CollectionStatus::Complete);
    captured.native_coverage = Some(SourceState::Partial);
    health.collection(&captured).unwrap();
    health
        .with(|state| {
            state.schedules.values_mut().next().unwrap().last_native_observation =
                Some(Instant::now() - Duration::from_secs(3600));
        })
        .unwrap();
    let mut metadata = event(CollectionStatus::Complete);
    metadata.work = FleetWork::SkipCommitted;
    metadata.native_coverage = Some(SourceState::Collected);
    health.collection(&metadata).unwrap();
    let metrics = String::from_utf8(health.reply().unwrap().metrics).unwrap();
    assert!(metrics.contains("state=\"partial\""));
    assert!(!metrics.contains("state=\"collected\""));
    assert!(
        metrics.lines().any(|line| line.starts_with("culprit_collector_schedule_last_native_observation_age_seconds{")
            && line.ends_with(" 3600"))
    );
}

#[test]
fn missing_native_evidence_on_a_later_capture_is_unknown_instead_of_reusing_old_healthy_scope() {
    let health = health();
    let mut captured = event(CollectionStatus::Complete);
    captured.native_coverage = Some(SourceState::Collected);
    health.collection(&captured).unwrap();
    health.collection(&event(CollectionStatus::Incomplete(CollectionStop::SourceUnavailable))).unwrap();
    let metrics = String::from_utf8(health.reply().unwrap().metrics).unwrap();
    assert!(!metrics.lines().any(|line| line.starts_with("culprit_collector_schedule_native_coverage{")));
    assert!(metrics.contains("last_native_observation_age_seconds"), "the original observation clock remains visible");
}
