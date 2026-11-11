use super::*;
fn seconds(value: u64) -> Duration {
    Duration::from_secs(value)
}
fn inventory() -> FleetInventory {
    FleetInventory {
        targets: ["a", "b", "c"]
            .into_iter()
            .map(|name| FleetTarget {
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
