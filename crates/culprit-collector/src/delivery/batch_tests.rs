//! A slow transfer remains bounded per batch and acknowledged progress survives a journal restart.
use super::{
    contract::Contract,
    driver::{DeliveryDriver, DeliveryPolicy},
    evidence::*,
    file_sink::FileTelemetrySink,
    file_store::FileRecoveryStore,
    journal::DeliveryJournal,
    recovery::*,
    sink::*,
    tests::contract,
};
use crate::collection_files::ObservationFiles;
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Transfers {
    store: Arc<FileRecoveryStore>,
    sink: FileTelemetrySink,
    staged: Mutex<Vec<String>>,
    submitted: Mutex<Vec<String>>,
}
#[async_trait]
impl RecoveryStore for Transfers {
    async fn pending_ranges(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        self.store.pending_ranges(cursor, limit).await
    }
    async fn retire(&self, fence: &Fence, publication: &PublishedRange) -> Result<()> {
        self.store.retire(fence, publication).await
    }
    async fn activity_heads(&self, cursor: Option<&str>, limit: u32) -> Result<ActivityHeadPage> {
        self.store.activity_heads(cursor, limit).await
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
    let policy = ActivityPolicy::default();
    let request = CollectionRequest {
        selection: CollectionSelection::Activity { policy: policy.clone() },
        budget: CollectionBudget::default(),
    };
    let capture = CaptureIdentity {
        observation_id: "quiet-activity".into(),
        revision: Some(CaptureRevision { outbox_id: "outbox".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let source = CollectionDescription {
        database: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        ),
        source: "activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: "native-epoch".into(),
            at: "2020-01-01T06:00:00Z".into(),
            database_wide: true,
            policy,
            native_rows: 0,
            qualifying: 0,
            observations: 0,
            blocking_edges: 0,
            details: ActivityDetailStatus::NotNeeded,
        }),
    };
    let path = root.join("capture");
    let mut writer = ObservationFiles::create(&path, request, capture).unwrap();
    writer.describe(source).unwrap();
    for index in 0..4 {
        assert!(writer.write_page(index, &[]).unwrap().is_none());
        writer.seal(index).unwrap();
    }
    writer.confirm_source(SourceCompletion::Verified).unwrap();
    PreparedRange::from_validated(&path, &writer.finish(4).unwrap(), contract(), vec![]).unwrap()
}
#[tokio::test]
async fn transfers_larger_than_one_operation_budget_resume_at_the_next_batch_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let range = quiet_activity(root.path());
    let id = range.manifest().range_id.clone();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let transfers = Transfers {
        sink: FileTelemetrySink::create(&root.path().join("sink"), contract(), store.clone()).unwrap(),
        store,
        staged: Mutex::new(vec![]),
        submitted: Mutex::new(vec![]),
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
        assert_eq!(submitted.len(), 3); // Checkpoints are recovery-only; the other successful empty streams are explicit.
        assert_eq!(submitted.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);
    }
    let heads = RecoveryStore::activity_heads(&transfers, None, 10).await.unwrap();
    assert_eq!(heads.heads.len(), 1);
    assert_eq!(heads.heads[0].range_id, id);
    let reopened = FileRecoveryStore::open(&root.path().join("recovery"), contract()).unwrap();
    assert_eq!(reopened.activity_heads(None, 10).await.unwrap().heads, heads.heads);
}
