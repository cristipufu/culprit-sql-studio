use super::*;
pub(super) fn health() -> CollectorHealth {
    CollectorHealth::new(
        &FleetInventory {
            targets: vec![FleetTarget {
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
    let mut event = FleetCollectionEvent {
        connection: "private-connection-canary".into(),
        schedule: "private-schedule-canary".into(),
        work: FleetWork::Capture,
        status: CollectionStatus::Incomplete(CollectionStop::SourceUnavailable),
        capture: None,
        error_code: None,
        gap: None,
    };
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
