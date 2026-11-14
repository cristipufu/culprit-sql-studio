use super::*;
use crate::fleet::scheduler::{ADMISSION_START_WINDOW, Outcome};

fn clock(seconds: i64) -> chrono::DateTime<chrono::Utc> {
    now() + chrono::TimeDelta::seconds(seconds)
}
fn snapshot_inventory() -> FleetInventory {
    inventory(ScheduledCollection::Snapshot { dataset: SnapshotDataset::Settings })
}
async fn initialize(journal: &FleetJournal, scheduler: &mut Scheduler) {
    let state = scheduler.initial_state(Duration::ZERO, clock(0)).unwrap();
    scheduler.persisted(journal.initialize_scheduler(state).await.unwrap()).unwrap();
}
async fn reserve(journal: &FleetJournal, scheduler: &mut Scheduler, elapsed: u64, wall: i64) -> Job {
    let job = scheduler.take(Duration::from_secs(elapsed)).unwrap().unwrap();
    let state = scheduler.admission_checkpoint(Duration::from_secs(elapsed), clock(wall)).unwrap();
    scheduler.persisted(journal.update_scheduler(state).await.unwrap()).unwrap();
    job
}
async fn complete(journal: &FleetJournal, scheduler: &mut Scheduler, elapsed: u64, wall: i64, outcome: Outcome) {
    scheduler.finish(Duration::from_secs(elapsed), outcome).unwrap();
    let state = scheduler.completion_checkpoint(Duration::from_secs(elapsed), clock(wall)).unwrap();
    scheduler.persisted(journal.update_scheduler(state).await.unwrap()).unwrap();
}
async fn restore(journal: &FleetJournal, inventory: &FleetInventory, wall: i64) -> Scheduler {
    let mut scheduler = Scheduler::new(inventory, "one").unwrap();
    scheduler.restore(journal.scheduler().await.unwrap().unwrap(), Duration::ZERO, clock(wall)).unwrap();
    if let Some(checkpoint) = scheduler.resumed_admission_checkpoint(Duration::ZERO, clock(wall)).unwrap() {
        scheduler.persisted(journal.update_scheduler(checkpoint).await.unwrap()).unwrap();
    }
    scheduler
}

#[tokio::test]
async fn a_mid_admission_restart_keeps_timeout_shutdown_cadence_and_physical_resource_reservations() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("fleet");
    let mut inventory = snapshot_inventory();
    let mut alias = inventory.targets[0].clone();
    alias.id = "same-physical-resource".into();
    alias.connection = "another-connection".into();
    inventory.targets.push(alias);
    let journal = FleetJournal::open(&directory, &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    reserve(&journal, &mut scheduler, 0, 0).await;
    drop(journal);
    let reopened = FleetJournal::open(&directory, &inventory, "one").await.unwrap();
    let mut resumed = restore(&reopened, &inventory, 3_600).await;
    let reserved = ADMISSION_START_WINDOW
        + Duration::from_millis(u64::from(inventory.targets[0].schedules[0].budget.total_timeout_ms))
        + crate::capture::SOURCE_SHUTDOWN_TIMEOUT
        + Duration::from_secs(60);
    assert!(resumed.take(reserved - Duration::from_millis(1)).unwrap().is_none());
    assert_eq!(resumed.take(reserved).unwrap().unwrap().target, "same-physical-resource");
    let mut restarted_again = restore(&reopened, &inventory, 3_601).await;
    assert!(
        restarted_again.take(reserved - Duration::from_secs(2)).unwrap().is_none(),
        "the first restart must durably rebase the old in-progress reservation"
    );
    assert_eq!(
        restarted_again.take(reserved - Duration::from_secs(1)).unwrap().unwrap().target,
        "same-physical-resource"
    );
}

#[tokio::test]
async fn a_restart_cannot_round_a_fractional_cooldown_down_to_an_earlier_admission() {
    let root = tempfile::tempdir().unwrap();
    let inventory = snapshot_inventory();
    let journal = FleetJournal::open(&root.path().join("fleet"), &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    reserve(&journal, &mut scheduler, 0, 0).await;
    scheduler.finish(Duration::from_nanos(1), Outcome::Complete).unwrap();
    let state = scheduler.completion_checkpoint(Duration::from_nanos(2), clock(0)).unwrap();
    scheduler.persisted(journal.update_scheduler(state).await.unwrap()).unwrap();

    let mut resumed = restore(&journal, &inventory, 0).await;
    assert!(resumed.take(Duration::from_secs(60) - Duration::from_nanos(1)).unwrap().is_none());
    assert!(resumed.take(Duration::from_secs(60)).unwrap().is_some());
}

#[tokio::test]
async fn restarting_preserves_pressure_backoff_and_serves_an_independent_resource_before_the_waiting_alias() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("fleet");
    let mut inventory = snapshot_inventory();
    for (id, resource) in [("second", "pool"), ("independent", "other")] {
        let mut target = inventory.targets[0].clone();
        target.id = id.into();
        target.connection = id.into();
        target.admission_group = resource.into();
        inventory.targets.push(target);
    }
    let journal = FleetJournal::open(&directory, &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    assert_eq!(reserve(&journal, &mut scheduler, 0, 0).await.target, "fixture");
    complete(&journal, &mut scheduler, 10, 10, Outcome::Pressure).await;
    drop(journal);
    let reopened = FleetJournal::open(&directory, &inventory, "one").await.unwrap();
    let mut resumed = restore(&reopened, &inventory, 20).await;
    assert_eq!(reserve(&reopened, &mut resumed, 0, 20).await.target, "independent");
    complete(&reopened, &mut resumed, 0, 20, Outcome::Complete).await;
    assert!(resumed.take(Duration::from_secs(49)).unwrap().is_none());
    assert_eq!(reserve(&reopened, &mut resumed, 50, 70).await.target, "second");
    complete(&reopened, &mut resumed, 60, 80, Outcome::Pressure).await;
    let mut again = restore(&reopened, &inventory, 90).await;
    // The independent source has already served its turn; only the pressured resource is held for 120 s.
    let independent = reserve(&reopened, &mut again, 0, 90).await;
    assert_eq!(independent.target, "independent");
    complete(&reopened, &mut again, 0, 90, Outcome::Complete).await;
    assert!(again.take(Duration::from_secs(59)).unwrap().is_none());
    assert_eq!(reserve(&reopened, &mut again, 60, 150).await.target, "independent");
    complete(&reopened, &mut again, 60, 150, Outcome::Complete).await;
    assert!(again.take(Duration::from_secs(109)).unwrap().is_none());
    assert_eq!(again.take(Duration::from_secs(110)).unwrap().unwrap().target, "fixture");
}

#[tokio::test]
async fn failed_job_backoff_and_history_family_cadence_survive_restart() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::QueryHistory { lookback_seconds: 86_400, reconcile_seconds: 3_600 });
    let journal = FleetJournal::open(&root.path().join("fleet"), &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    let current = reserve(&journal, &mut scheduler, 0, 0).await;
    complete(&journal, &mut scheduler, 0, 0, Outcome::Failed).await;
    let mut resumed = restore(&journal, &inventory, 0).await;
    assert!(resumed.take(Duration::from_secs(59)).unwrap().is_none(), "backfill cannot bypass its family's cadence");
    let backfill = reserve(&journal, &mut resumed, 60, 60).await;
    assert_ne!(backfill.key, current.key);
    complete(&journal, &mut resumed, 60, 60, Outcome::Complete).await;
    let mut again = restore(&journal, &inventory, 60).await;
    assert_eq!(reserve(&journal, &mut again, 60, 120).await.key, current.key);
    complete(&journal, &mut again, 60, 120, Outcome::Failed).await;
    let state = journal.scheduler().await.unwrap().unwrap();
    let saved = serde_json::to_value(&state.jobs[&current.key]).unwrap();
    assert_eq!(saved["failures"], 2, "serving another lane cannot reset this job's failure history");
    assert_eq!(saved["delay"]["remaining_ms"], 120_000);
}

#[tokio::test]
async fn backward_wall_time_never_earns_cooldown_and_invalid_or_contradictory_state_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let inventory = snapshot_inventory();
    let journal = FleetJournal::open(&root.path().join("fleet"), &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    reserve(&journal, &mut scheduler, 0, 0).await;
    complete(&journal, &mut scheduler, 10, 10, Outcome::Complete).await;
    let mut later = restore(&journal, &inventory, 40).await;
    assert!(later.take(Duration::from_secs(29)).unwrap().is_none());
    assert!(
        later.take(Duration::from_secs(30)).unwrap().is_some(),
        "verified wall elapsed can finish a durable cooldown"
    );
    let mut resumed = restore(&journal, &inventory, -3_600).await;
    assert!(resumed.take(Duration::from_secs(59)).unwrap().is_none());
    assert!(resumed.take(Duration::from_secs(60)).unwrap().is_some());
    let mut fresh = Scheduler::new(&inventory, "one").unwrap();
    let mut state = journal.scheduler().await.unwrap().unwrap();
    state.clock.wall = chrono::DateTime::from_timestamp(-1, 0).unwrap();
    assert!(fresh.restore(state, Duration::ZERO, clock(20)).is_err());
    let mut fresh = Scheduler::new(&inventory, "one").unwrap();
    let mut state = journal.scheduler().await.unwrap().unwrap();
    let key = state.jobs.keys().next().unwrap().clone();
    let mut corrupted = serde_json::to_value(&state.jobs[&key]).unwrap();
    corrupted["delay"]["remaining_ms"] = serde_json::json!(u64::MAX);
    state.jobs.insert(key, serde_json::from_value(corrupted).unwrap());
    assert!(
        fresh.restore(state, Duration::ZERO, clock(20)).is_err(),
        "a delay outside the configured policy is corrupt"
    );
    let mut fresh = Scheduler::new(&inventory, "one").unwrap();
    let mut state = journal.scheduler().await.unwrap().unwrap();
    state.jobs.clear();
    assert!(
        fresh.restore(state, Duration::ZERO, clock(20)).is_err(),
        "missing jobs cannot become immediately runnable defaults"
    );
    let mut fresh = Scheduler::new(&inventory, "one").unwrap();
    let mut state = journal.scheduler().await.unwrap().unwrap();
    let resource = state.resources.values().next().unwrap().clone();
    state.resources.insert("unconfigured".into(), resource);
    assert!(fresh.restore(state, Duration::ZERO, clock(20)).is_err());
    let mut fresh = Scheduler::new(&inventory, "one").unwrap();
    let mut state = journal.scheduler().await.unwrap().unwrap();
    let key = state.jobs.keys().next().unwrap().clone();
    let mut corrupted = serde_json::to_value(&state.jobs[&key]).unwrap();
    corrupted["failures"] = serde_json::json!(2);
    state.jobs.insert(key, serde_json::from_value(corrupted).unwrap());
    assert!(fresh.restore(state, Duration::ZERO, clock(20)).is_err());
    let mut fresh = Scheduler::new(&inventory, "one").unwrap();
    assert!(fresh.initial_state(Duration::ZERO, chrono::DateTime::from_timestamp(-1, 0).unwrap()).is_err());
}

#[tokio::test]
async fn skipped_committed_work_restores_its_original_native_resource_admission() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::QueryHistory { lookback_seconds: 86_400, reconcile_seconds: 3_600 });
    let journal = FleetJournal::open(&root.path().join("fleet"), &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    let before = journal.scheduler().await.unwrap().unwrap();
    reserve(&journal, &mut scheduler, 0, 0).await;
    complete(&journal, &mut scheduler, 0, 0, Outcome::Skipped { more: true }).await;
    let saved = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(&saved.resources).unwrap(), serde_json::to_value(&before.resources).unwrap());
    assert_eq!(serde_json::to_value(&saved.families).unwrap(), serde_json::to_value(&before.families).unwrap());
    let mut resumed = restore(&journal, &inventory, 0).await;
    assert!(resumed.take(Duration::ZERO).unwrap().is_some(), "metadata-only progress does not charge native work");
}

#[tokio::test]
async fn stale_scheduler_writes_and_corrupt_saved_rows_cannot_open_a_native_source() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("fleet");
    let inventory = snapshot_inventory();
    let journal = FleetJournal::open(&directory, &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    let mut stale = restore(&journal, &inventory, 0).await;
    reserve(&journal, &mut scheduler, 0, 0).await;
    stale.take(Duration::ZERO).unwrap().unwrap();
    let stale_update = stale.admission_checkpoint(Duration::ZERO, clock(0)).unwrap();
    assert!(journal.update_scheduler(stale_update).await.is_err());
    let connection = rusqlite::Connection::open(directory.join("schedule.sqlite3")).unwrap();
    connection.execute("UPDATE admission_jobs SET state_json='{}'", []).unwrap();
    drop(connection);
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    let mut runner = FleetRunner::new(&backend, &inventory, "one", &journal, &mut outbox).unwrap();
    assert!(runner.run(&CollectionCancellation::default(), Some(1), |_| Ok(())).await.is_err());
    assert!(backend.requests.lock().unwrap().is_empty());
    assert_eq!(*backend.discoveries.lock().unwrap(), 0);
}

#[tokio::test]
async fn failed_admission_storage_never_dispatches_native_work_or_commits_a_partial_scheduler_update() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("fleet");
    let inventory = snapshot_inventory();
    let journal = FleetJournal::open(&directory, &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    let before = journal.scheduler().await.unwrap().unwrap();
    let connection = rusqlite::Connection::open(directory.join("schedule.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_admission BEFORE UPDATE ON admission_jobs BEGIN SELECT RAISE(ABORT,'owned write fault'); END;").unwrap();
    drop(connection);
    let backend = Backend::new();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let mut runner = FleetRunner::new(&backend, &inventory, "one", &journal, &mut outbox).unwrap();
    assert!(runner.run(&CollectionCancellation::default(), Some(1), |_| Ok(())).await.is_err());
    assert!(backend.requests.lock().unwrap().is_empty());
    assert_eq!(*backend.discoveries.lock().unwrap(), 0);
    let after = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(after.clock.revision, before.clock.revision);
    assert_eq!(after.clock.sequence, before.clock.sequence);
    assert_eq!(serde_json::to_value(&after.resources).unwrap(), serde_json::to_value(&before.resources).unwrap());
    assert_eq!(serde_json::to_value(&after.families).unwrap(), serde_json::to_value(&before.families).unwrap());
    assert!(
        runner.run(&CollectionCancellation::default(), Some(1), |_| Ok(())).await.is_err(),
        "a failed persistence owner cannot resume from uncommitted in-memory state"
    );
}

#[tokio::test]
async fn native_interval_discovery_and_capture_use_separate_paced_admissions() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::QueryHistory { lookback_seconds: 7_200, reconcile_seconds: 7_200 });
    let journal = FleetJournal::open(&root.path().join("fleet"), &inventory, "one").await.unwrap();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    initialize(&journal, &mut scheduler).await;
    let backend = Backend::new();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let job = reserve(&journal, &mut scheduler, 0, 0).await;
    let admitted = admission::AdmittedBackend::new(&backend, tokio::time::Instant::now());
    let result = work::execute(
        &admitted,
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &CollectionCancellation::default(),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(result.event.work, FleetWork::DiscoverIntervals);
    assert_eq!(*backend.discoveries.lock().unwrap(), 1);
    assert!(backend.requests.lock().unwrap().is_empty());
    complete(&journal, &mut scheduler, 0, 0, result.outcome).await;
    assert!(scheduler.take(Duration::from_secs(59)).unwrap().is_none());
    let job = reserve(&journal, &mut scheduler, 60, 60).await;
    let admitted = admission::AdmittedBackend::new(&backend, tokio::time::Instant::now());
    let result = work::execute(
        &admitted,
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &CollectionCancellation::default(),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(result.event.work, FleetWork::Capture);
    assert_eq!(*backend.discoveries.lock().unwrap(), 1);
    assert_eq!(backend.requests.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn repeated_run_calls_keep_one_monotonic_origin_and_cancellation_during_wait_does_not_read() {
    let root = tempfile::tempdir().unwrap();
    let inventory = snapshot_inventory();
    let journal = FleetJournal::open(&root.path().join("fleet"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    let mut runner = FleetRunner::new(&backend, &inventory, "one", &journal, &mut outbox).unwrap();
    assert_eq!(runner.run(&CollectionCancellation::default(), Some(1), |_| Ok(())).await.unwrap().complete, 1);
    let cancellation = CollectionCancellation::default();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        cancel.cancel();
    });
    assert_eq!(runner.run(&cancellation, Some(1), |_| Ok(())).await.unwrap().attempts, 0);
    assert_eq!(backend.requests.lock().unwrap().len(), 1);
    let next = tokio::time::timeout(
        Duration::from_secs(31),
        runner.run(&CollectionCancellation::default(), Some(1), |_| Ok(())),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(next.complete, 1, "a new run does not restart the original sixty-second cooldown");
    assert_eq!(backend.requests.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_turn_admits_only_one_native_call_and_slow_durability_cannot_authorize_a_late_start() {
    let root = tempfile::tempdir().unwrap();
    let backend = Backend::new();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: CollectionBudget::default(),
    };
    let admitted = admission::AdmittedBackend::new(&backend, tokio::time::Instant::now());
    admitted.capture("fixture", request.clone(), &mut outbox, &CollectionCancellation::default()).await.unwrap();
    let error = admitted
        .capture("fixture", request.clone(), &mut outbox, &CollectionCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    let expired = admission::AdmittedBackend::new(&backend, tokio::time::Instant::now());
    tokio::time::advance(ADMISSION_START_WINDOW).await;
    let error = expired.capture("fixture", request, &mut outbox, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert_eq!(backend.requests.lock().unwrap().len(), 1);
}
