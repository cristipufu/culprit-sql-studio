//! A slow transfer remains bounded per batch and acknowledged progress survives a journal restart.
use super::{
    contract::Contract,
    driver::{DeliveryDriver, DeliveryPolicy},
    evidence::*,
    file_sink::FileTelemetrySink,
    file_store::FileRecoveryStore,
    journal::{DeliveryJournal, HydrationPriority},
    recovery::*,
    sink::*,
    tests::contract,
};
use crate::collection_files::ObservationFiles;
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Transfers {
    store: Arc<FileRecoveryStore>,
    sink: FileTelemetrySink,
    staged: Mutex<Vec<String>>,
    submitted: Mutex<Vec<String>>,
    pending_unavailable: AtomicBool,
    pending_calls: AtomicUsize,
}
#[async_trait]
impl RecoveryStore for Transfers {
    async fn pending_ranges(&self, cursor: Option<&str>, limit: u32, order: RecoveryOrder) -> Result<RecoveryPage> {
        self.pending_calls.fetch_add(1, Ordering::SeqCst);
        if self.pending_unavailable.load(Ordering::SeqCst) {
            return Err(DeliveryProblem::Transport.error());
        }
        self.store.pending_ranges(cursor, limit, order).await
    }
    async fn pending_diagnostic_inputs(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        self.store.pending_diagnostic_inputs(cursor, limit).await
    }
    async fn diagnostic_input_pending(&self, input: &DiagnosticCaptureRef, version: u32) -> Result<bool> {
        self.store.diagnostic_input_pending(input, version).await
    }
    async fn retire(&self, fence: &Fence, publication: &PublishedRange) -> Result<()> {
        self.store.retire(fence, publication).await
    }
    async fn capture_manifest(&self, capture: &culprit_core::telemetry::CaptureIdentity) -> Result<RangeManifest> {
        self.store.capture_manifest(capture).await
    }
    async fn diagnostic_heads(&self, cursor: Option<&str>, limit: u32) -> Result<super::recovery::DiagnosticHeadPage> {
        self.store.diagnostic_heads(cursor, limit).await
    }
    fn id(&self) -> &str {
        self.store.id()
    }
    async fn checkpoint_head(&self) -> Result<Option<String>> {
        self.store.checkpoint_head().await
    }
    async fn captured_range(&self, range: &CapturedRange) -> Result<Option<CaptureIdentity>> {
        self.store.captured_range(range).await
    }
    async fn stage(&self, batch: &PayloadObject, path: &Path) -> Result<StagedBatch> {
        self.staged.lock().unwrap().push(path.file_name().unwrap().to_string_lossy().into());
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.store.stage(batch, path).await
    }
    async fn commit(
        &self,
        fence: &Fence,
        head: Option<&str>,
        manifest: &RangeManifest,
        batches: &[StagedBatch],
    ) -> Result<CommitReceipt> {
        self.store.commit(fence, head, manifest, batches).await
    }
    async fn load(&self, id: &str) -> Result<RangeManifest> {
        self.store.load(id).await
    }
    async fn recover(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        self.store.recover(cursor, limit).await
    }
    async fn open_object(&self, batch: &PayloadObject) -> Result<RecoveryReader> {
        self.store.open_object(batch).await
    }
}
#[async_trait]
impl TelemetrySink for Transfers {
    async fn validate_contract(&self, contract: &Contract) -> Result<()> {
        self.sink.validate_contract(contract).await
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch: &str) -> Result<Submission> {
        self.submitted.lock().unwrap().push(batch.into());
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.sink.dispatch(range, attempt, batch).await
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, limit: u32) -> Result<ReceiptPage> {
        self.sink.receipts(range, cursor, limit).await
    }
    async fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        self.sink.publish(range, attempt).await
    }
    async fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        self.sink.publication_visible(publication, range).await
    }
    async fn poll_updates(&self, cursor: Option<&str>, limit: u32) -> Result<DeliveryUpdates> {
        self.sink.poll_updates(cursor, limit).await
    }
}
pub(super) fn quiet_activity(root: &Path) -> PreparedRange {
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget::default(),
    };
    let capture = CaptureIdentity {
        observation_id: "quiet-activity".into(),
        revision: Some(CaptureRevision { outbox_id: "outbox".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        source: "activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            epoch: Some("native-epoch".into()),
            observed_from: "2020-01-01T06:00:00Z".into(),
            observed_until: "2020-01-01T06:00:01Z".into(),
            at: "2020-01-01T06:00:00Z".into(),
            scope: culprit_core::MeasurementScope::Database,
            native_rows: 0,
            observations: 0,
            coverage: StateSourceCoverage::collected("activity".into(), 0),
        }),
    };
    let path = root.join("capture");
    let mut writer = ObservationFiles::create(&path, request, capture).unwrap();
    writer.describe(source).unwrap();
    for index in 0..writer.manifest.files.len() {
        assert!(writer.write_page(index, &[]).unwrap().is_none());
        writer.seal(index).unwrap();
    }
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    PreparedRange::from_validated(&path, &writer.finish(4).unwrap(), contract(), vec![]).unwrap()
}
fn empty_query_history(root: &Path) -> PreparedRange {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::QueryHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T07:00:00Z".into(),
        },
        budget: Default::default(),
    };
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        source: "query-store".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            interval: TelemetryInterval {
                native_id: "1".into(),
                from: "2020-01-01T06:00:00Z".into(),
                until: "2020-01-01T07:00:00Z".into(),
            },
            dimensions: Default::default(),
            native_properties: Default::default(),
            waits: None,
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "history".into(),
        revision: Some(CaptureRevision { outbox_id: "outbox".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let path = root.join("history");
    let mut writer = ObservationFiles::create(&path, request, capture).unwrap();
    writer.describe(source).unwrap();
    for index in 0..4 {
        writer.seal(index).unwrap();
    }
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    PreparedRange::from_validated(&path, &writer.finish(4).unwrap(), contract(), vec![]).unwrap()
}
pub(super) fn empty_diagnostics(root: &Path, parent: &super::evidence::RangeManifest, sequence: u64) -> PreparedRange {
    empty_diagnostics_for_version(root, parent, sequence, DIAGNOSTIC_CONTROLLER_VERSION)
}
pub(super) fn empty_diagnostics_for_version(
    root: &Path,
    parent: &super::evidence::RangeManifest,
    sequence: u64,
    controller_version: u32,
) -> PreparedRange {
    let parent = super::diagnostic_recovery::capture_reference(parent).unwrap();
    let step_id = digest(format!("step-{sequence}").as_bytes());
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Diagnostics {
            parent: parent.clone(),
            controller_version,
            step_id: step_id.clone(),
        },
        budget: Default::default(),
    };
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        source: DIAGNOSTICS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
            parent: parent.clone(),
            controller_version,
            step_id,
            analyzed_at: "2020-01-01T06:03:00Z".into(),
            records: 0,
            checkpoint_records: 1,
        }),
    };
    let mut part = DiagnosticCheckpointPart {
        id: String::new(),
        controller_version,
        target: source.target.clone(),
        input: parent,
        part: 0,
        parts: 1,
        episodes: vec![],
    };
    part.id = part.expected_id().unwrap();
    let capture = CaptureIdentity {
        observation_id: format!("diagnostic-{sequence}"),
        revision: Some(CaptureRevision {
            outbox_id: "outbox".into(),
            owner_epoch: "1".into(),
            sequence: sequence.to_string(),
        }),
    };
    let path = root.join(&capture.observation_id);
    let mut writer = ObservationFiles::create(&path, request, capture).unwrap();
    writer.describe(source).unwrap();
    writer.seal(0).unwrap();
    writer.begin_page(1).unwrap();
    assert!(writer.write_page(1, &[TelemetryRecord::DiagnosticCheckpoint(part)]).unwrap().is_none());
    writer.seal(1).unwrap();
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    PreparedRange::from_validated(&path, &writer.finish(0).unwrap(), contract(), vec![]).unwrap()
}
// Transfer latency belongs to virtual time; filesystem scheduling must not spend the synthetic request budget.
#[tokio::test(start_paused = true)]
async fn transfers_larger_than_one_operation_budget_resume_at_the_next_batch_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let range = empty_query_history(root.path());
    let id = range.manifest().range_id.clone();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let transfers = Transfers {
        sink: FileTelemetrySink::create(&root.path().join("sink"), contract(), store.clone()).unwrap(),
        store,
        staged: Mutex::new(vec![]),
        submitted: Mutex::new(vec![]),
        pending_unavailable: AtomicBool::new(false),
        pending_calls: AtomicUsize::new(0),
    };
    let policy = DeliveryPolicy { request_timeout_ms: 1000, visibility_delay_ms: 1, ..Default::default() };
    let path = root.path().join("delivery");
    let mut journal = DeliveryJournal::create(&path, contract()).unwrap();
    journal.register(&range).await.unwrap();
    let mut now = 0;
    // Reopen the journal after every turn, including between stage acknowledgements and dispatch acknowledgements.
    let mut retired = false;
    for _ in 0..32 {
        let state = {
            let mut capabilities = crate::delivery::committed_cache::CommittedRangeCache::default();
            let mut driver =
                DeliveryDriver::new(&mut journal, &transfers, &transfers, &mut capabilities, policy).unwrap();
            driver.step(&id, owner.fence(), now).await.unwrap()
        };
        now += 1000;
        if state.phase == DeliveryPhase::Retired {
            retired = true;
            break;
        }
        drop(journal);
        journal = DeliveryJournal::open(&path, contract()).unwrap();
    }
    assert!(retired);
    {
        let staged = transfers.staged.lock().unwrap();
        assert_eq!(staged.len(), 4);
        assert_eq!(staged.iter().collect::<std::collections::BTreeSet<_>>().len(), 4);
        let submitted = transfers.submitted.lock().unwrap();
        assert_eq!(submitted.len(), 4); // All four query-history streams have explicit successful empty receipts.
        assert_eq!(submitted.iter().collect::<std::collections::BTreeSet<_>>().len(), 4);
    }
    assert!(transfers.diagnostic_heads(None, 10).await.unwrap().heads.is_empty());
    let reopened = FileRecoveryStore::open(&root.path().join("recovery"), contract()).unwrap();
    assert_eq!(reopened.load(&id).await.unwrap(), *range.manifest());
    assert!(reopened.pending_ranges(None, 10, RecoveryOrder::OldestFirst).await.unwrap().ranges.is_empty());
}

#[tokio::test]
async fn an_unavailable_pending_index_does_not_block_already_committed_analytical_delivery() {
    use super::worker::{DeliveryJournals, DeliveryWorker};
    use crate::{capture::CollectionCancellation, outbox::Outbox};
    let root = tempfile::tempdir().unwrap();
    let prepared = super::tests::prepared(root.path(), 1);
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let committed = stage_and_commit(&*store, owner.fence(), None, &prepared).await.unwrap();
    let transfers = Transfers {
        sink: FileTelemetrySink::create(&root.path().join("sink"), contract(), store.clone()).unwrap(),
        store: store.clone(),
        staged: Mutex::new(vec![]),
        submitted: Mutex::new(vec![]),
        pending_unavailable: AtomicBool::new(true),
        pending_calls: AtomicUsize::new(0),
    };
    let mut outbox = Outbox::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    let mut archive = DeliveryJournal::create(&root.path().join("archive"), contract()).unwrap();
    let mut analytical = DeliveryJournal::create(&root.path().join("analytical"), contract()).unwrap();
    analytical.restore(&committed, HydrationPriority::Historical).await.unwrap();
    let mut worker = DeliveryWorker::new(
        outbox.delivery(),
        DeliveryJournals { archive: &mut archive, analytical: &mut analytical },
        &transfers,
        &transfers,
        owner.fence().clone(),
        contract(),
        DeliveryPolicy { visibility_delay_ms: 1, retry_delay_ms: 1, ..Default::default() },
    )
    .unwrap();
    let mut published = false;
    for now in 0..16 {
        worker
            .work_once(now * 1000, &CollectionCancellation::default(), |_, result| {
                match result {
                    Ok(entry) => published |= matches!(entry.phase, DeliveryPhase::Published | DeliveryPhase::Retired),
                    Err(error) => {
                        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Transport)))
                    }
                }
                Ok(())
            })
            .await
            .unwrap();
        if published {
            break;
        }
    }
    assert!(
        published,
        "pending-index discovery is not a prerequisite for delivering locally registered committed work"
    );
    assert_eq!(transfers.sink.physical_rows(&committed.manifest().range_id).await.unwrap(), 1);
    read_publication(&transfers, &committed).await.unwrap();
}

#[tokio::test]
async fn metadata_backpressure_keeps_bounded_verified_candidates_without_repeated_remote_reads() {
    use super::worker::{DeliveryJournals, DeliveryWorker};
    use crate::{capture::CollectionCancellation, outbox::Outbox};
    let root = tempfile::tempdir().unwrap();
    let first = super::tests::prepared(root.path(), 1);
    let second = super::tests::prepared(root.path(), 2);
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let first = stage_and_commit(&*store, owner.fence(), None, &first).await.unwrap();
    let second = stage_and_commit(&*store, owner.fence(), Some(&first.manifest().range_id), &second).await.unwrap();
    let transfers = Transfers {
        sink: FileTelemetrySink::create(&root.path().join("sink"), contract(), store.clone()).unwrap(),
        store: store.clone(),
        staged: Mutex::new(vec![]),
        submitted: Mutex::new(vec![]),
        pending_unavailable: AtomicBool::new(false),
        pending_calls: AtomicUsize::new(0),
    };
    let mut outbox = Outbox::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    let mut archive = DeliveryJournal::create(&root.path().join("archive"), contract()).unwrap();
    let mut analytical = DeliveryJournal::create(&root.path().join("analytical"), contract()).unwrap();
    analytical.restore(&first, HydrationPriority::Historical).await.unwrap();
    let accounting = rusqlite::Connection::open(root.path().join("analytical/delivery.sqlite3")).unwrap();
    // Simulate an already charged metadata set without writing hundreds of MiB in a unit test.
    accounting
        .execute(
            "UPDATE delivery_manifests SET bytes=?1",
            [i64::try_from(super::journal::MAX_IMMUTABLE_BYTES).unwrap()],
        )
        .unwrap();
    accounting
        .execute("UPDATE deliveries SET state=json_set(state,'$.phase','quarantined'),phase='quarantined'", [])
        .unwrap();
    let mut worker = DeliveryWorker::new(
        outbox.delivery(),
        DeliveryJournals { archive: &mut archive, analytical: &mut analytical },
        &transfers,
        &transfers,
        owner.fence().clone(),
        contract(),
        DeliveryPolicy::default(),
    )
    .unwrap();
    for now in [0, 1000, 2000, 3000, 4000, 7000] {
        worker
            .work_once(now, &CollectionCancellation::default(), |_, result| {
                result.unwrap();
                Ok(())
            })
            .await
            .unwrap();
    }
    assert_eq!(
        transfers.pending_calls.load(Ordering::SeqCst),
        3,
        "a quota-deferred manifest must not be downloaded on every turn"
    );
    transfers.pending_unavailable.store(true, Ordering::SeqCst);
    accounting.execute("UPDATE delivery_manifests SET bytes=length(CAST(manifest AS BLOB))", []).unwrap();
    worker
        .work_once(13_000, &CollectionCancellation::default(), |_, result| {
            result.unwrap();
            Ok(())
        })
        .await
        .unwrap();
    drop(worker);
    assert_eq!(transfers.pending_calls.load(Ordering::SeqCst), 3);
    assert_eq!(analytical.status(&second.manifest().range_id).await.unwrap().phase, DeliveryPhase::DispatchPending);
}

#[tokio::test]
async fn cold_recovery_admits_fresh_commits_without_traversing_the_historical_backlog() {
    use super::worker::{DeliveryJournals, DeliveryWorker};
    use crate::{capture::CollectionCancellation, outbox::Outbox};
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let mut head = None;
    for sequence in 1..=40 {
        let prepared = super::tests::prepared(root.path(), sequence);
        let committed = stage_and_commit(&*store, owner.fence(), head.as_deref(), &prepared).await.unwrap();
        head = Some(committed.manifest().range_id.clone());
    }
    let newest = head.unwrap();
    let sink = FileTelemetrySink::create(&root.path().join("sink"), contract(), store.clone()).unwrap();
    let mut outbox = Outbox::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    let mut archive = DeliveryJournal::create(&root.path().join("archive"), contract()).unwrap();
    let mut analytical = DeliveryJournal::create(&root.path().join("analytical"), contract()).unwrap();
    let mut worker = DeliveryWorker::new(
        outbox.delivery(),
        DeliveryJournals { archive: &mut archive, analytical: &mut analytical },
        &*store,
        &sink,
        owner.fence().clone(),
        contract(),
        DeliveryPolicy::default(),
    )
    .unwrap();
    for now in [0, 250] {
        worker.work_once(now, &CollectionCancellation::default(), |_, result| result.map(|_| ())).await.unwrap();
    }
    let fresh = super::tests::prepared(root.path(), 41);
    let fresh = stage_and_commit(&*store, owner.fence(), Some(&newest), &fresh).await.unwrap();
    // The remote commit is not a local notification. Periodic discovery must also interrupt an old traversal.
    worker.work_once(60_001, &CollectionCancellation::default(), |_, result| result.map(|_| ())).await.unwrap();
    drop(worker);
    assert!(
        analytical.status(&newest).await.is_ok(),
        "current committed observations must be admitted while cold recovery still owns a historical cursor"
    );
    assert!(analytical.status(&fresh.manifest().range_id).await.is_ok());
}
