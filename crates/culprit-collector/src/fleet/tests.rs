use super::*;
use crate::fleet::{
    scheduler::{Job, state_failed},
    work::Checkpoint,
};
use culprit_core::{DatabaseIdentity, Engine};
use std::sync::Mutex;
mod remote_receipts;

struct Backend {
    requests: Mutex<Vec<CollectionRequest>>,
    discoveries: Mutex<u32>,
    failure: Mutex<Option<CollectionStop>>,
    database: Mutex<DatabaseIdentity>,
    status: Mutex<CollectionStatus>,
}
impl Backend {
    fn new() -> Self {
        Self {
            requests: Mutex::new(vec![]),
            discoveries: Mutex::new(0),
            failure: Mutex::new(None),
            database: Mutex::new(DatabaseIdentity::server(Engine::Mssql, "fixture", "loopback.invalid", 1433, None)),
            status: Mutex::new(CollectionStatus::Complete),
        }
    }
}
#[async_trait]
impl FleetBackend for Backend {
    fn validate_target(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn discover(&self, _: &str, request: &IntervalDiscoveryRequest) -> Result<IntervalDiscoveryPage> {
        *self.discoveries.lock().unwrap() += 1;
        let page = IntervalDiscoveryPage {
            database: self.database.lock().unwrap().clone(),
            source: "query_store".into(),
            observed_at: "2026-11-10T12:00:00Z".into(),
            metadata_from: Some(request.from.clone()),
            intervals: vec![interval(10), interval(11)],
            next_cursor: None,
        };
        assert!(page.matches_request(request));
        Ok(page)
    }
    async fn capture(
        &self,
        _: &str,
        request: CollectionRequest,
        _: &mut Outbox,
        _: &CollectionCancellation,
    ) -> Result<CollectionManifest> {
        self.requests.lock().unwrap().push(request.clone());
        if let Some(failure) = self.failure.lock().unwrap().take() {
            return Err(failure.error());
        }
        let observation = match &request.selection {
            CollectionSelection::QueryHistory { from, .. } => {
                SourceObservation::QueryHistory(QueryHistoryDescription {
                    interval: if work::parse_time(from).unwrap().hour() == 10 { interval(10) } else { interval(11) },
                    dimensions: [("replica_group_id".into(), vec!["0".into()])].into(),
                    native_properties: [
                        ("capture_mode".into(), "ALL".into()),
                        ("waits_capture_enabled".into(), (true).to_string()),
                    ]
                    .into(),
                })
            }
            CollectionSelection::Snapshot { dataset } => SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: *dataset,
                observed_from: "2026-11-10T12:00:00Z".into(),
                observed_until: "2026-11-10T12:00:01Z".into(),
                records: 0,
                sources: vec![StateSourceCoverage::collected("fixture".into(), 0)],
            }),
            CollectionSelection::ResourceHistory { from, until } => {
                SourceObservation::RetainedSamples(SampleHistoryDescription {
                    from: from.clone(),
                    until: until.clone(),
                    observed_at: "2026-11-10T12:00:00Z".into(),
                    available_from: Some(from.clone()),
                    available_until: Some(until.clone()),
                    expected_samples: 0,
                    nominal_period_ms: 15_000,
                })
            }
            _ => return Err(state_failed()),
        };
        Ok(CollectionManifest {
            format_version: EXPORT_FORMAT_VERSION,
            capture: CaptureIdentity {
                observation_id: format!("test-{}", self.requests.lock().unwrap().len()),
                revision: None,
            },
            started_at: "2026-11-10T12:00:00Z".into(),
            finished_at: Some("2026-11-10T12:00:01Z".into()),
            request,
            description: Some(CollectionDescription {
                target: self.database.lock().unwrap().clone().into(),
                source: "query_store".into(),
                continuity: SourceContinuity::Unverified,
                observation,
            }),
            status: self.status.lock().unwrap().clone(),
            source_queries: 1,
            read_metrics: None,
            directory_synced: true,
            files: vec![],
            references: ReferenceCoverage::default(),
        })
    }
}
use chrono::Timelike;
fn interval(hour: u32) -> TelemetryInterval {
    TelemetryInterval {
        native_id: hour.to_string(),
        from: format!("2026-11-10T{hour:02}:00:00Z"),
        until: format!("2026-11-10T{:02}:00:00Z", hour + 1),
    }
}
fn inventory(collection: ScheduledCollection) -> FleetInventory {
    FleetInventory {
        targets: vec![FleetTarget {
            connection: "fixture".into(),
            shard: "one".into(),
            admission_group: "pool".into(),
            schedules: vec![FleetSchedule {
                id: "test".into(),
                every_seconds: 60,
                jitter_seconds: 0,
                collection,
                budget: CollectionBudget::default(),
            }],
        }],
    }
}
fn job(inventory: &FleetInventory) -> Job {
    Scheduler::new(inventory, "one").unwrap().take(Duration::ZERO).unwrap().unwrap()
}
fn now() -> chrono::DateTime<chrono::Utc> {
    work::parse_time("2026-11-10T12:00:00Z").unwrap()
}
async fn outbox(path: &std::path::Path) -> Outbox {
    let mut value = Outbox::create(path, OutboxLimits::default()).unwrap();
    value.recover().await.unwrap();
    value
}

#[tokio::test]
async fn history_discovery_failed_captures_and_remaining_intervals_survive_restart_without_rediscovery() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::QueryHistory { lookback_seconds: 7_200, reconcile_seconds: 7_200 });
    let job = job(&inventory);
    let backend = Backend::new();
    let cancellation = CollectionCancellation::default();
    let state = root.path().join("state");
    let journal = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let first = work::execute(&backend, &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    assert_eq!(first.event.work, FleetWork::DiscoverIntervals);
    assert_eq!(*backend.discoveries.lock().unwrap(), 1);
    *backend.failure.lock().unwrap() = Some(CollectionStop::TimedOut);
    let failed = work::execute(&backend, &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    assert_eq!(failed.event.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
    work::execute(&backend, &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    drop(journal);
    let reopened = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    work::execute(&backend, &reopened, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    let requests = backend.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0], requests[1]);
    assert_ne!(requests[1], requests[2]);
    assert_eq!(*backend.discoveries.lock().unwrap(), 1);
    assert!(reopened.load(&job.key).await.unwrap().history.is_none());
}

#[tokio::test]
async fn assignment_changes_and_overlapping_local_owners_are_refused_without_changing_saved_work() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut inventory = inventory(ScheduledCollection::Snapshot { dataset: SnapshotDataset::Settings });
    let job = job(&inventory);
    let journal = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    let checkpoint = Checkpoint { last_until: Some("2026-11-10T00:00:00Z".into()), ..Default::default() };
    journal.save(&job.key, &checkpoint).await.unwrap();
    assert!(FleetJournal::open(&state, &inventory, "one").await.is_err());
    drop(journal);
    inventory.targets[0].admission_group = "different-pool".into();
    assert!(FleetJournal::open(&state, &inventory, "one").await.is_err());
    inventory.targets[0].admission_group = "pool".into();
    let reopened = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    assert_eq!(reopened.load(&job.key).await.unwrap().last_until, checkpoint.last_until);
}

#[tokio::test]
async fn an_uncertain_native_shutdown_stops_the_runner_before_any_other_source_is_opened() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::Snapshot { dataset: SnapshotDataset::Settings });
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    *backend.failure.lock().unwrap() = Some(CollectionStop::CancellationUnconfirmed);
    let mut runner = FleetRunner::new(&backend, &inventory, "one", &journal, &mut outbox).unwrap();
    let error = runner.run(&CollectionCancellation::default(), Some(3), |_| Ok(())).await.unwrap_err();
    assert_eq!(
        error.detail,
        Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed))
    );
    assert_eq!(backend.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_database_recreated_during_a_history_scan_cannot_inherit_the_old_cursor() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::QueryHistory { lookback_seconds: 7_200, reconcile_seconds: 7_200 });
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    let job = job(&inventory);
    let cancellation = CollectionCancellation::default();
    work::execute(&backend, &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    let before = serde_json::to_value(journal.load(&job.key).await.unwrap()).unwrap();
    backend.database.lock().unwrap().incarnation = Some("new-generation".into());
    assert!(
        work::execute(&backend, &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
            .await
            .is_err()
    );
    assert_eq!(serde_json::to_value(journal.load(&job.key).await.unwrap()).unwrap(), before);
}

#[tokio::test]
async fn coverage_gaps_survive_checkpoint_reopen_and_repeated_receipts_do_not_duplicate_them() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let inventory = inventory(ScheduledCollection::Snapshot { dataset: SnapshotDataset::Settings });
    let key = job(&inventory).key;
    let journal = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    let gap = FleetGap {
        id: digest(b"gap"),
        connection: "fixture".into(),
        schedule: "test".into(),
        range: TelemetryRange::TimeWindow { from: "2026-11-01T00:00:00Z".into(), until: "2026-11-02T00:00:00Z".into() },
        reason: FleetGapReason::BackfillLimit,
        recorded_at: "2026-11-10T00:00:00Z".into(),
        capture: None,
    };
    let checkpoint = Checkpoint { last_until: Some("2026-11-02T00:00:00Z".into()), ..Default::default() };
    journal.save_progress(&key, &checkpoint, Some(gap.clone())).await.unwrap();
    journal.save_progress(&key, &checkpoint, Some(gap.clone())).await.unwrap();
    drop(journal);
    let reopened = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    let page = reopened.gaps(None, 10).await.unwrap();
    assert_eq!(page.gaps, std::slice::from_ref(&gap));
    assert!(reopened.gaps(Some(&page.cursor), 10).await.unwrap().gaps.is_empty());
    let mut later = gap.clone();
    later.id = "0".repeat(64);
    reopened.save_progress(&key, &checkpoint, Some(later.clone())).await.unwrap();
    assert_eq!(reopened.gaps(Some(&page.cursor), 10).await.unwrap().gaps, [later]);
    assert_eq!(reopened.load(&key).await.unwrap().last_until, checkpoint.last_until);
}

#[tokio::test]
async fn failed_windows_retry_identically_and_unrecoverable_retention_advances_only_with_a_durable_gap() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::ResourceHistory { window_seconds: 300, overlap_seconds: 30 });
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    let job = job(&inventory);
    journal
        .save(&job.key, &Checkpoint { last_until: Some("2026-10-01T00:00:00Z".into()), ..Default::default() })
        .await
        .unwrap();
    let cancellation = CollectionCancellation::default();
    *backend.failure.lock().unwrap() = Some(CollectionStop::TimedOut);
    work::execute(&backend, &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    assert_eq!(journal.load(&job.key).await.unwrap().last_until.as_deref(), Some("2026-10-01T00:00:00Z"));
    assert!(journal.gaps(None, 10).await.unwrap().gaps.is_empty());
    work::execute(&backend, &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    let requests = backend.requests.lock().unwrap().clone();
    assert_eq!(requests[0], requests[1]);
    let before = journal.load(&job.key).await.unwrap().last_until;
    let gap = journal.gaps(None, 10).await.unwrap();
    assert_eq!(gap.gaps[0].reason, FleetGapReason::BackfillLimit);
    *backend.status.lock().unwrap() = CollectionStatus::Incomplete(CollectionStop::RetentionGap);
    let result = work::execute(&backend, &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    assert_eq!(result.event.status, CollectionStatus::Incomplete(CollectionStop::RetentionGap));
    assert_ne!(journal.load(&job.key).await.unwrap().last_until, before);
    assert_eq!(journal.gaps(Some(&gap.cursor), 10).await.unwrap().gaps[0].reason, FleetGapReason::NativeRetention);
}
