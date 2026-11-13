use super::*;
fn seconds(value: u64) -> Duration {
    Duration::from_secs(value)
}
fn inventory() -> FleetInventory {
    FleetInventory {
        targets: ["a", "b", "c"]
            .into_iter()
            .map(|name| FleetTarget {
                id: name.into(),
                subject: culprit_core::telemetry::CollectionSubject::Database,
                connection: name.into(),
                shard: "one".into(),
                admission_group: if name == "c" { "separate" } else { "shared" }.into(),
                schedules: vec![FleetSchedule {
                    id: "live".into(),
                    every_seconds: 60,
                    jitter_seconds: 0,
                    collection: ScheduledCollection::Activity { policy: ActivityPolicy::default() },
                    budget: CollectionBudget::default(),
                }],
            })
            .collect(),
    }
}
#[test]
fn shared_resource_cooldown_leaves_independent_targets_runnable_and_preserves_fairness() {
    let mut scheduler = Scheduler::new(&inventory(), "one").unwrap();
    assert_eq!(scheduler.take(seconds(0)).unwrap().unwrap().connection, "a");
    assert!(scheduler.take(seconds(0)).is_err());
    scheduler.finish(seconds(10), Outcome::More).unwrap();
    assert_eq!(scheduler.take(seconds(10)).unwrap().unwrap().connection, "c");
    scheduler.finish(seconds(10), Outcome::Complete).unwrap();
    assert!(scheduler.take(seconds(69)).unwrap().is_none());
    assert_eq!(scheduler.take(seconds(70)).unwrap().unwrap().connection, "b");
    scheduler.finish(seconds(70), Outcome::Pressure).unwrap();
    assert_eq!(scheduler.take(seconds(70)).unwrap().unwrap().connection, "c");
    scheduler.finish(seconds(70), Outcome::Complete).unwrap();
    assert_eq!(scheduler.take(seconds(130)).unwrap().unwrap().connection, "a");
}
#[test]
fn missing_ticks_never_create_a_catchup_burst_or_shorter_minimum_cadence() {
    let mut value = inventory();
    value.targets.truncate(1);
    value.targets[0].schedules[0].every_seconds = 3_600;
    value.targets[0].schedules[0].jitter_seconds = 300;
    let mut scheduler = Scheduler::new(&value, "one").unwrap();
    scheduler.take(seconds(86_400)).unwrap().unwrap();
    scheduler.finish(seconds(86_410), Outcome::Complete).unwrap();
    assert!(scheduler.take(seconds(90_009)).unwrap().is_none());
    assert_eq!(failure_delay(3_600, 99), seconds(3_600));
    assert_eq!(failure_delay(60, 99), seconds(900));
    assert!(scheduler.finish(seconds(90_010), Outcome::Failed).is_err());
}

#[test]
fn pending_native_work_retries_at_its_cadence_without_growing_failure_backoff() {
    let mut inventory = inventory();
    inventory.targets.truncate(1);
    inventory.targets[0].schedules[0].every_seconds = 180;
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    for attempt in 0..8 {
        let started = seconds(attempt * 190);
        assert!(scheduler.take(started).unwrap().is_some());
        scheduler.finish(started + seconds(10), Outcome::More).unwrap();
        assert!(scheduler.take(started + seconds(189)).unwrap().is_none());
    }
}
#[test]
fn resource_pressure_holds_the_entire_group_with_bounded_backoff() {
    let mut value = inventory();
    value.targets.truncate(2);
    let mut scheduler = Scheduler::new(&value, "one").unwrap();
    for time in [0, 60, 180, 420] {
        scheduler.take(seconds(time)).unwrap().unwrap();
        scheduler.finish(seconds(time), Outcome::Pressure).unwrap();
    }
    assert!(scheduler.take(seconds(899)).unwrap().is_none());
    assert!(scheduler.take(seconds(900)).unwrap().is_some());
}

#[test]
fn database_and_pool_jobs_share_route_admission_but_have_independent_turns_and_checkpoints() {
    let mut inventory = inventory();
    inventory.targets.truncate(1);
    let mut pool = inventory.targets[0].clone();
    pool.id = "pool-owner".into();
    pool.subject = CollectionSubject::Shared {
        resource: SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new("fixture-server"),
            native: NativeIdentity::new("fixture-pool"),
        },
    };
    pool.schedules[0].collection = ScheduledCollection::ResourceHistory { window_seconds: 300, overlap_seconds: 30 };
    inventory.targets.push(pool);
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    let database = scheduler.take(seconds(0)).unwrap().unwrap();
    assert_eq!(database.target, "a");
    assert_eq!(database.subject, CollectionSubject::Database);
    scheduler.finish(seconds(10), Outcome::Complete).unwrap();
    assert!(scheduler.take(seconds(69)).unwrap().is_none());
    let pool = scheduler.take(seconds(70)).unwrap().unwrap();
    assert_eq!(database.connection, pool.connection);
    assert_eq!(database.schedule.id, pool.schedule.id);
    assert_eq!(database.resource, pool.resource);
    assert_eq!(pool.target, "pool-owner");
    assert!(matches!(pool.subject, CollectionSubject::Shared { .. }));
    assert_ne!(database.key, pool.key);
    assert_ne!(database.family, pool.family);
    scheduler.finish(seconds(70), Outcome::Pressure).unwrap();
    assert!(scheduler.take(seconds(129)).unwrap().is_none());
    assert_eq!(scheduler.take(seconds(130)).unwrap().unwrap().target, "a");
}

#[test]
fn recent_history_and_backfill_keep_independent_turns_and_durable_job_keys() {
    let mut inventory = inventory();
    inventory.targets.truncate(1);
    inventory.targets[0].schedules[0].collection =
        ScheduledCollection::QueryHistory { lookback_seconds: 86_400, reconcile_seconds: 3_600 };
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    let recent = scheduler.take(seconds(0)).unwrap().unwrap();
    assert!(recent.lane == HistoryLane::Current);
    scheduler.finish(seconds(0), Outcome::More).unwrap();
    let backfill = scheduler.take(seconds(60)).unwrap().unwrap();
    assert!(backfill.lane == HistoryLane::Backfill);
    assert_ne!(recent.key, backfill.key);
    scheduler.finish(seconds(60), Outcome::More).unwrap();
    assert!(scheduler.take(seconds(120)).unwrap().unwrap().lane == HistoryLane::Current);
}

#[test]
fn splitting_history_into_lanes_cannot_accelerate_its_configured_native_cadence() {
    let mut inventory = inventory();
    inventory.targets.truncate(1);
    inventory.targets[0].schedules[0].every_seconds = 600;
    inventory.targets[0].schedules[0].collection =
        ScheduledCollection::QueryHistory { lookback_seconds: 86_400, reconcile_seconds: 3_600 };
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    scheduler.take(seconds(0)).unwrap().unwrap();
    scheduler.finish(seconds(10), Outcome::More).unwrap();
    assert!(scheduler.take(seconds(609)).unwrap().is_none());
    assert!(scheduler.take(seconds(610)).unwrap().unwrap().lane == HistoryLane::Backfill);
}

#[test]
fn frequent_observations_do_not_wait_behind_an_entire_catalog_sweep_and_never_accelerate_resource_admission() {
    let mut value = inventory();
    value.targets.truncate(1);
    let catalog_count = 24;
    for index in 0..catalog_count {
        value.targets.push(FleetTarget {
            id: format!("catalog-database-{index}"),
            subject: culprit_core::telemetry::CollectionSubject::Database,
            connection: format!("catalog-database-{index}"),
            shard: "one".into(),
            admission_group: "shared".into(),
            schedules: vec![FleetSchedule {
                id: format!("catalog-{index}"),
                every_seconds: 3600,
                jitter_seconds: 0,
                collection: ScheduledCollection::Snapshot { dataset: SnapshotDataset::Settings },
                budget: CollectionBudget::default(),
            }],
        });
    }
    let mut scheduler = Scheduler::new(&value, "one").unwrap();
    let mut activity_times = vec![];
    let mut catalogs = std::collections::BTreeSet::new();
    for minute in 0..120 {
        let now = seconds(minute * 60);
        let job = scheduler.take(now).unwrap().unwrap();
        if job.schedule.collection.collection() == TelemetryCollection::Activity {
            activity_times.push(minute * 60);
        } else {
            catalogs.insert(job.schedule.id);
        }
        scheduler.finish(now, Outcome::Complete).unwrap();
        assert!(scheduler.take(now + seconds(59)).unwrap().is_none());
    }
    assert!(activity_times.len() >= 50);
    assert!(activity_times.windows(2).all(|pair| pair[1] - pair[0] <= 180), "{activity_times:?}");
    assert_eq!(catalogs.len(), catalog_count, "infrequent sources must still receive service");
}
