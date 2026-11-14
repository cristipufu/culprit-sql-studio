use super::*;
use crate::fleet::{
    admission::AdmittedBackend,
    scheduler::{Job, Work, state_failed},
    work::Checkpoint,
};
use culprit_core::{DatabaseIdentity, Engine};
use std::sync::Mutex;
mod remote_receipts;
mod scheduler_persistence;

struct Backend {
    requests: Mutex<Vec<CollectionRequest>>,
    discoveries: Mutex<u32>,
    failure: Mutex<Option<CollectionStop>>,
    database: Mutex<DatabaseIdentity>,
    target_override: Mutex<Option<CollectionTarget>>,
    resource_bounds: Mutex<Option<(String, String)>>,
    status: Mutex<CollectionStatus>,
    state_sources: Mutex<Option<Vec<StateSourceCoverage>>>,
}
impl Backend {
    fn new() -> Self {
        Self {
            requests: Mutex::new(vec![]),
            discoveries: Mutex::new(0),
            failure: Mutex::new(None),
            database: Mutex::new(DatabaseIdentity::server(Engine::Mssql, "fixture", "loopback.invalid", 1433, None)),
            target_override: Mutex::new(None),
            resource_bounds: Mutex::new(None),
            status: Mutex::new(CollectionStatus::Complete),
            state_sources: Mutex::new(None),
        }
    }
}
#[async_trait]
impl FleetBackend for Backend {
    fn validate_target(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn discover(
        &self,
        _: &str,
        request: &IntervalDiscoveryRequest,
        admission: FleetSourceAdmission,
    ) -> Result<IntervalDiscoveryPage> {
        admission.window.check_start()?;
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
        admission: FleetSourceAdmission,
    ) -> FleetCaptureOutcome {
        let result = async {
            admission.window.check_start()?;
            self.requests.lock().unwrap().push(request.clone());
            if let Some(failure) = self.failure.lock().unwrap().take() {
                return Err(failure.error());
            }
            let observation = match &request.selection {
                CollectionSelection::QueryHistory { from, .. } => {
                    SourceObservation::QueryHistory(QueryHistoryDescription {
                        waits: None,
                        interval: if work::parse_time(from).unwrap().hour() == 10 {
                            interval(10)
                        } else {
                            interval(11)
                        },
                        dimensions: [("replica_group_id".into(), vec!["0".into()])].into(),
                        native_properties: [
                            ("capture_mode".into(), "ALL".into()),
                            ("waits_capture_enabled".into(), (true).to_string()),
                        ]
                        .into(),
                    })
                }
                CollectionSelection::Snapshot { dataset } => {
                    let sources = self
                        .state_sources
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| vec![StateSourceCoverage::collected("fixture".into(), 0)]);
                    SourceObservation::Snapshot(StateSnapshotDescription {
                        dataset: *dataset,
                        observed_from: "2026-11-10T12:00:00Z".into(),
                        observed_until: "2026-11-10T12:00:01Z".into(),
                        records: sources.iter().map(|source| source.records).sum(),
                        sources,
                    })
                }
                CollectionSelection::ResourceHistory { from, until } => {
                    let (available_from, available_until) =
                        self.resource_bounds.lock().unwrap().clone().unwrap_or_else(|| (from.clone(), until.clone()));
                    SourceObservation::RetainedSamples(SampleHistoryDescription {
                        from: from.clone(),
                        until: until.clone(),
                        observed_at: "2026-11-10T12:00:00Z".into(),
                        available_from: Some(available_from),
                        available_until: Some(available_until),
                        expected_samples: 0,
                        nominal_period_ms: Some(15_000),
                    })
                }
                _ => return Err(state_failed()),
            };
            let target = self.target_override.lock().unwrap().clone().unwrap_or_else(|| match &request.subject {
                CollectionSubject::Database => self.database.lock().unwrap().clone().into(),
                CollectionSubject::Shared { resource } => CollectionTarget::Shared(resource.clone()),
            });
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
                    target,
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
        .await;
        FleetCaptureOutcome { result, source_work: SourceWorkState::Quiescent }
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
            id: "fixture".into(),
            subject: culprit_core::telemetry::CollectionSubject::Database,
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
fn periodic_job(work: Work) -> Job {
    let Work::Scheduled(job) = work else { panic!("expected a configured periodic job") };
    job
}
fn admitted(backend: &dyn FleetBackend) -> AdmittedBackend<'_> {
    AdmittedBackend::new(backend, tokio::time::Instant::now())
}
fn job(inventory: &FleetInventory) -> Job {
    periodic_job(Scheduler::new(inventory, "one").unwrap().take(Duration::ZERO).unwrap().unwrap())
}
fn now() -> chrono::DateTime<chrono::Utc> {
    work::parse_time("2026-11-10T12:00:00Z").unwrap()
}
async fn outbox(path: &std::path::Path) -> Outbox {
    let mut value = Outbox::create(path, OutboxLimits::default()).unwrap();
    value.recover().await.unwrap();
    value
}

fn database_and_pool_inventory() -> FleetInventory {
    let mut inventory = inventory(ScheduledCollection::ResourceHistory { window_seconds: 300, overlap_seconds: 30 });
    let mut pool = inventory.targets[0].clone();
    pool.id = "pool-owner".into();
    pool.subject = CollectionSubject::Shared {
        resource: SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new("fixture-server"),
            native: NativeIdentity::new("fixture-pool"),
        },
    };
    inventory.targets.push(pool);
    inventory
}

fn subject_jobs(inventory: &FleetInventory) -> [Job; 2] {
    let mut scheduler = Scheduler::new(inventory, "one").unwrap();
    let database = periodic_job(scheduler.take(Duration::ZERO).unwrap().unwrap());
    scheduler.finish(Duration::ZERO, scheduler::Outcome::Complete).unwrap();
    let pool = periodic_job(scheduler.take(Duration::from_secs(60)).unwrap().unwrap());
    [database, pool]
}

#[tokio::test]
async fn complete_snapshot_operations_keep_unavailable_partial_and_empty_native_coverage_distinct() {
    use culprit_core::SourceState;
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::Snapshot { dataset: SnapshotDataset::Operational });
    let job = job(&inventory);
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    for (sources, expected) in [
        (
            vec![StateSourceCoverage::unavailable("native".into(), culprit_core::ErrorCode::PermissionDenied, None)],
            SourceState::Unavailable,
        ),
        (vec![StateSourceCoverage::not_applicable("native".into())], SourceState::NotApplicable),
        (vec![StateSourceCoverage::observed("native".into(), 0, false)], SourceState::Partial),
        (vec![StateSourceCoverage::collected("native".into(), 0)], SourceState::Empty),
        (vec![StateSourceCoverage::collected("native".into(), 1)], SourceState::Collected),
    ] {
        *backend.state_sources.lock().unwrap() = Some(sources);
        let result = work::execute(
            &admitted(&backend),
            &journal,
            &mut CaptureStorage::local(&mut outbox),
            &job,
            &CollectionCancellation::default(),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(result.event.status, CollectionStatus::Complete);
        assert_eq!(result.event.native_coverage, Some(expected));
        assert!(!result.pause_all);
        assert!(journal.load(&job.key).await.unwrap().pending.is_none());

        *backend.status.lock().unwrap() = CollectionStatus::Incomplete(CollectionStop::RowLimit);
        let interrupted = work::execute(
            &admitted(&backend),
            &journal,
            &mut CaptureStorage::local(&mut outbox),
            &job,
            &CollectionCancellation::default(),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(interrupted.event.status, CollectionStatus::Incomplete(CollectionStop::RowLimit));
        assert_eq!(
            interrupted.event.native_coverage,
            Some(match expected {
                SourceState::Collected | SourceState::Empty => SourceState::Partial,
                other => other,
            }),
            "interrupted capture must retain known source unavailability or inapplicability"
        );
        *backend.status.lock().unwrap() = CollectionStatus::Complete;
    }
}

#[tokio::test]
async fn one_connection_keeps_subject_requests_checkpoints_and_retention_gaps_separate_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let inventory = database_and_pool_inventory();
    let jobs = subject_jobs(&inventory);
    assert_eq!(jobs[0].connection, jobs[1].connection);
    assert_ne!(jobs[0].key, jobs[1].key);
    let state = root.path().join("state");
    let journal = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    *backend.status.lock().unwrap() = CollectionStatus::Incomplete(CollectionStop::RetentionGap);
    *backend.resource_bounds.lock().unwrap() = Some(("2026-11-10T11:59:00Z".into(), "2026-11-10T12:00:00Z".into()));
    for job in &jobs {
        let result = work::execute(
            &admitted(&backend),
            &journal,
            &mut CaptureStorage::local(&mut outbox),
            job,
            &CollectionCancellation::default(),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(result.event.target, job.target);
        assert_eq!(result.event.status, CollectionStatus::Incomplete(CollectionStop::RetentionGap));
        assert_eq!(backend.requests.lock().unwrap().last().unwrap().subject, job.subject);
    }
    drop(journal);
    let reopened = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    for job in &jobs {
        let checkpoint = reopened.load(&job.key).await.unwrap();
        assert!(checkpoint.pending.is_none());
        assert!(job.subject.accepts(checkpoint.target.as_ref().unwrap()));
        assert_eq!(checkpoint.last_until.as_deref(), Some("2026-11-10T12:00:00+00:00"));
    }
    let gaps = reopened.gaps(None, 10).await.unwrap().gaps;
    assert_eq!(gaps.len(), 2);
    assert_eq!(gaps[0].connection, gaps[1].connection);
    assert_ne!(gaps[0].target, gaps[1].target);
    assert_ne!(gaps[0].id, gaps[1].id);
}

#[tokio::test]
async fn a_wrong_native_pool_result_cannot_pin_or_advance_the_expected_subject() {
    let root = tempfile::tempdir().unwrap();
    let inventory = database_and_pool_inventory();
    let [_, job] = subject_jobs(&inventory);
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    let CollectionSubject::Shared { mut resource } = job.subject.clone() else { unreachable!() };
    resource.native = NativeIdentity::new("unrelated-pool");
    *backend.target_override.lock().unwrap() = Some(CollectionTarget::Shared(resource));
    let result = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &CollectionCancellation::default(),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(result.event.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
    let checkpoint = journal.load(&job.key).await.unwrap();
    assert!(checkpoint.target.is_none());
    assert!(checkpoint.last_until.is_none());
    assert_eq!(checkpoint.pending.unwrap().subject, job.subject);
}

#[tokio::test]
async fn an_unobserved_tail_retries_the_exact_window_until_native_samples_close_it() {
    let root = tempfile::tempdir().unwrap();
    let inventory = database_and_pool_inventory();
    let [_, job] = subject_jobs(&inventory);
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    *backend.status.lock().unwrap() = CollectionStatus::Incomplete(CollectionStop::SourceNotReady);
    *backend.resource_bounds.lock().unwrap() = Some(("2026-11-10T11:30:00Z".into(), "2026-11-10T11:59:50Z".into()));
    let cancellation = CollectionCancellation::default();
    let result = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &cancellation,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(result.event.status, CollectionStatus::Incomplete(CollectionStop::SourceNotReady));
    assert!(matches!(result.outcome, scheduler::Outcome::More));
    let pending = journal.load(&job.key).await.unwrap();
    assert!(pending.last_until.is_none());
    assert!(pending.pending.is_some());
    assert!(journal.gaps(None, 10).await.unwrap().gaps.is_empty());
    *backend.failure.lock().unwrap() = Some(CollectionStop::SourceNotReady);
    let retry = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &cancellation,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(retry.event.status, CollectionStatus::Incomplete(CollectionStop::SourceNotReady));
    assert!(matches!(retry.outcome, scheduler::Outcome::More));
    *backend.status.lock().unwrap() = CollectionStatus::Complete;
    *backend.resource_bounds.lock().unwrap() = Some(("2026-11-10T11:30:00Z".into(), "2026-11-10T12:00:00Z".into()));
    let later = now() + chrono::TimeDelta::minutes(5);
    let result = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &cancellation,
        later,
    )
    .await
    .unwrap();
    assert_eq!(result.event.status, CollectionStatus::Complete);
    {
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests[0], requests[1]);
        assert_eq!(requests[1], requests[2]);
    }
    let completed = journal.load(&job.key).await.unwrap();
    assert!(completed.pending.is_none());
    assert_eq!(completed.last_until.as_deref(), Some("2026-11-10T12:00:00+00:00"));
    assert!(journal.gaps(None, 10).await.unwrap().gaps.is_empty());
}

#[tokio::test]
async fn an_incorrect_retention_gap_cannot_skip_an_unobserved_tail() {
    let root = tempfile::tempdir().unwrap();
    let inventory = database_and_pool_inventory();
    let [_, job] = subject_jobs(&inventory);
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    *backend.status.lock().unwrap() = CollectionStatus::Incomplete(CollectionStop::RetentionGap);
    *backend.resource_bounds.lock().unwrap() = Some(("2026-11-10T11:30:00Z".into(), "2026-11-10T11:59:50Z".into()));
    let result = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &CollectionCancellation::default(),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(result.event.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
    let checkpoint = journal.load(&job.key).await.unwrap();
    assert!(checkpoint.pending.is_some());
    assert!(checkpoint.last_until.is_none());
    assert!(journal.gaps(None, 10).await.unwrap().gaps.is_empty());
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
    let first = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &cancellation,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(first.event.work, FleetWork::DiscoverIntervals);
    assert_eq!(*backend.discoveries.lock().unwrap(), 1);
    *backend.failure.lock().unwrap() = Some(CollectionStop::TimedOut);
    let failed = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &cancellation,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(failed.event.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
    work::execute(&admitted(&backend), &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    drop(journal);
    let reopened = FleetJournal::open(&state, &inventory, "one").await.unwrap();
    work::execute(&admitted(&backend), &reopened, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
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
    work::execute(&admitted(&backend), &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    let before = serde_json::to_value(journal.load(&job.key).await.unwrap()).unwrap();
    backend.database.lock().unwrap().incarnation = Some("new-generation".into());
    assert!(
        work::execute(
            &admitted(&backend),
            &journal,
            &mut CaptureStorage::local(&mut outbox),
            &job,
            &cancellation,
            now()
        )
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
        target: "fixture".into(),
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
    work::execute(&admitted(&backend), &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    assert_eq!(journal.load(&job.key).await.unwrap().last_until.as_deref(), Some("2026-10-01T00:00:00Z"));
    assert!(journal.gaps(None, 10).await.unwrap().gaps.is_empty());
    work::execute(&admitted(&backend), &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    let requests = backend.requests.lock().unwrap().clone();
    assert_eq!(requests[0], requests[1]);
    let before = journal.load(&job.key).await.unwrap().last_until;
    let gap = journal.gaps(None, 10).await.unwrap();
    assert_eq!(gap.gaps[0].reason, FleetGapReason::BackfillLimit);
    *backend.status.lock().unwrap() = CollectionStatus::Incomplete(CollectionStop::RetentionGap);
    *backend.resource_bounds.lock().unwrap() = Some(("2026-11-10T11:59:00Z".into(), "2026-11-10T12:00:00Z".into()));
    let result = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &cancellation,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(result.event.status, CollectionStatus::Incomplete(CollectionStop::RetentionGap));
    assert_ne!(journal.load(&job.key).await.unwrap().last_until, before);
    assert_eq!(journal.gaps(Some(&gap.cursor), 10).await.unwrap().gaps[0].reason, FleetGapReason::NativeRetention);
}
