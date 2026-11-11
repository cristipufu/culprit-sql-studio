use super::*;

fn schedule() -> FleetSchedule {
    FleetSchedule {
        id: "activity".into(),
        every_seconds: 60,
        jitter_seconds: 10,
        collection: ScheduledCollection::Activity { policy: ActivityPolicy::default() },
        budget: CollectionBudget::default(),
    }
}
fn target(connection: &str, shard: &str, group: &str) -> FleetTarget {
    FleetTarget {
        connection: connection.into(),
        shard: shard.into(),
        admission_group: group.into(),
        schedules: vec![schedule()],
    }
}
#[test]
fn one_physical_resource_cannot_be_split_between_static_owners() {
    let mut inventory = FleetInventory { targets: vec![target("a", "one", "pool"), target("b", "two", "pool")] };
    assert!(inventory.validate("one").is_err());
    inventory.targets[1].shard = "one".into();
    inventory.validate("one").unwrap();
    assert!(inventory.validate("missing").is_err());
    inventory.targets[1].connection = "a".into();
    assert!(inventory.validate("one").is_err());
}
#[test]
fn reject_duplicate_jobs_and_accelerated_or_unbounded_schedules() {
    let mut inventory = FleetInventory { targets: vec![target("a", "one", "pool")] };
    let mut duplicate = schedule();
    duplicate.id = "another-activity".into();
    inventory.targets[0].schedules.push(duplicate);
    assert!(inventory.validate("one").is_err());
    for (period, jitter) in [(0, 0), (59, 0), (86_401, 0), (60, 31)] {
        let mut job = schedule();
        job.every_seconds = period;
        job.jitter_seconds = jitter;
        assert!(job.validate().is_err());
    }
}
#[test]
fn history_windows_and_reconciliation_are_deliberately_bounded() {
    let mut job = schedule();
    for collection in [
        ScheduledCollection::QueryHistory { lookback_seconds: 3_600, reconcile_seconds: 59 },
        ScheduledCollection::QueryHistory { lookback_seconds: 3_600, reconcile_seconds: 3_601 },
        ScheduledCollection::ResourceHistory { window_seconds: 59, overlap_seconds: 0 },
        ScheduledCollection::DeadlockHistory { window_seconds: MAX_HISTORY_LOOKBACK_SECONDS, overlap_seconds: 1 },
    ] {
        job.collection = collection;
        assert!(job.validate().is_err());
    }
    job.collection = ScheduledCollection::QueryHistory { lookback_seconds: 86_400, reconcile_seconds: 3_600 };
    job.validate().unwrap();
    let mut unknown = serde_json::to_value(&job).unwrap();
    unknown["surprise"] = true.into();
    assert!(serde_json::from_value::<FleetSchedule>(unknown).is_err());
    let mut unknown = serde_json::to_value(&job.collection).unwrap();
    unknown["lookbak_seconds"] = 7_200.into();
    assert!(serde_json::from_value::<ScheduledCollection>(unknown).is_err());
}

#[test]
fn shared_defaults_are_explicit_after_parsing_and_misspelled_budget_values_are_rejected() {
    let json = serde_json::json!({"id":"live","every_seconds":60,"jitter_seconds":10,"collection":{"kind":"activity"}});
    let mut schedule: FleetSchedule = serde_json::from_value(json).unwrap();
    schedule.validate().unwrap();
    assert_eq!(schedule.budget, CollectionBudget::default());
    if let ScheduledCollection::Activity { policy } = &schedule.collection {
        assert_eq!(*policy, ActivityPolicy::default())
    } else {
        panic!("activity policy")
    }
    schedule.budget.statement_timeout_ms = 1_000;
    let mut wrong = serde_json::to_value(&schedule).unwrap();
    wrong["budget"]["statment_timeout_ms"] = 1_000.into();
    assert!(serde_json::from_value::<FleetSchedule>(wrong).is_err());
}
