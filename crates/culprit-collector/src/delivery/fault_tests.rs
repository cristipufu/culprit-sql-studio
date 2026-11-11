//! Hard process exits exercise durable intents before and after the external effects.
use super::{
    driver::*,
    file_sink::FileTelemetrySink,
    file_store::*,
    journal::DeliveryJournal,
    recovery::*,
    sink::*,
    tests::{contract, prepared, problem},
};
use async_trait::async_trait;
use culprit_core::{
    Result,
    telemetry::{DeliveryPhase, DeliveryProblem},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn crash(root: &Path, point: &str) {
    if std::env::var("CULPRIT_DELIVERY_CRASH_POINT").as_deref() == Ok(point) {
        std::fs::write(root.join("crash-reached"), point).unwrap();
        std::process::exit(73);
    }
}
struct CrashStore {
    inner: Arc<FileRecoveryStore>,
    root: PathBuf,
}
#[async_trait]
impl RecoveryStore for CrashStore {
    async fn pending_ranges(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        self.inner.pending_ranges(cursor, limit).await
    }
    async fn retire(&self, fence: &Fence, publication: &PublishedRange) -> Result<()> {
        self.inner.retire(fence, publication).await
    }
    async fn activity_heads(&self, cursor: Option<&str>, limit: u32) -> Result<ActivityHeadPage> {
        self.inner.activity_heads(cursor, limit).await
    }
    fn id(&self) -> &str {
        self.inner.id()
    }
    async fn checkpoint_head(&self) -> Result<Option<String>> {
        self.inner.checkpoint_head().await
    }
    async fn captured_range(
        &self,
        range: &culprit_core::telemetry::CapturedRange,
    ) -> Result<Option<culprit_core::telemetry::CaptureIdentity>> {
        self.inner.captured_range(range).await
    }
    async fn stage(&self, batch: &super::recovery::PayloadObject, path: &Path) -> Result<StagedBatch> {
        let result = self.inner.stage(batch, path).await?;
        crash(&self.root, "after-stage");
        Ok(result)
    }
    async fn commit(
        &self,
        fence: &Fence,
        head: Option<&str>,
        manifest: &super::evidence::RangeManifest,
        staged: &[StagedBatch],
    ) -> Result<CommitReceipt> {
        let result = self.inner.commit(fence, head, manifest, staged).await?;
        crash(&self.root, "after-commit");
        Ok(result)
    }
    async fn load(&self, id: &str) -> Result<super::evidence::RangeManifest> {
        self.inner.load(id).await
    }
    async fn recover(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        self.inner.recover(cursor, limit).await
    }
    async fn open_object(&self, batch: &super::recovery::PayloadObject) -> Result<super::recovery::RecoveryReader> {
        self.inner.open_object(batch).await
    }
}
struct CrashSink {
    inner: FileTelemetrySink,
    root: PathBuf,
}
#[async_trait]
impl TelemetrySink for CrashSink {
    async fn validate_contract(&self, contract: &super::contract::Contract) -> Result<()> {
        self.inner.validate_contract(contract).await
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission> {
        crash(&self.root, "before-dispatch");
        let result = self.inner.dispatch(range, attempt, batch_id).await?;
        crash(&self.root, "after-dispatch");
        Ok(result)
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, rows: u32) -> Result<ReceiptPage> {
        self.inner.receipts(range, cursor, rows).await
    }
    async fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        crash(&self.root, "before-publication");
        let result = self.inner.publish(range, attempt).await?;
        crash(&self.root, "after-publication");
        Ok(result)
    }
    async fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        self.inner.publication_visible(publication, range).await
    }
    async fn poll_updates(&self, cursor: Option<&str>, limit: u32) -> Result<DeliveryUpdates> {
        self.inner.poll_updates(cursor, limit).await
    }
}
#[test]
fn crash_child() {
    let Some(root) = std::env::var_os("CULPRIT_DELIVERY_CRASH_ROOT") else { return };
    let root = PathBuf::from(root);
    let id = std::env::var("CULPRIT_DELIVERY_RANGE").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let store = Arc::new(FileRecoveryStore::open(&root.join("recovery"), contract()).unwrap());
        let owner = store.acquire().await.unwrap();
        let recovery = CrashStore { inner: store.clone(), root: root.clone() };
        let sink = CrashSink {
            inner: FileTelemetrySink::open(&root.join("warehouse"), contract(), store).unwrap(),
            root: root.clone(),
        };
        let mut journal = DeliveryJournal::open(&root.join("delivery"), contract()).unwrap();
        let mut capabilities = crate::delivery::committed_cache::CommittedRangeCache::default();
        let mut driver = DeliveryDriver::new(
            &mut journal,
            &recovery,
            &sink,
            &mut capabilities,
            DeliveryPolicy { visibility_delay_ms: 1, ..Default::default() },
        )
        .unwrap();
        let now = std::env::var("CULPRIT_DELIVERY_NOW_MS").unwrap().parse::<i64>().unwrap();
        driver.step(&id, owner.fence(), now).await.unwrap();
    });
    std::process::exit(73);
}
#[tokio::test]
async fn process_death_across_delivery_boundaries_preserves_ids_and_cold_recovery() {
    for point in
        ["after-stage", "after-commit", "before-dispatch", "after-dispatch", "before-publication", "after-publication"]
    {
        let root = tempfile::TempDir::new().unwrap();
        let prepared = prepared(root.path(), 1);
        let id = prepared.manifest().range_id.clone();
        let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
        let sink = FileTelemetrySink::create(&root.path().join("warehouse"), contract(), store).unwrap();
        drop(sink);
        let journal = DeliveryJournal::create(&root.path().join("delivery"), contract()).unwrap();
        journal.register(&prepared).await.unwrap();
        drop(journal);
        let mut reached = false;
        let mut retired = false;
        for iteration in 0..20 {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", "delivery::fault_tests::crash_child", "--nocapture"])
                .env("CULPRIT_DELIVERY_CRASH_ROOT", root.path())
                .env("CULPRIT_DELIVERY_RANGE", &id)
                .env("CULPRIT_DELIVERY_NOW_MS", (1_000_000 + iteration * 1000).to_string());
            if !reached {
                child.env("CULPRIT_DELIVERY_CRASH_POINT", point);
            } else {
                child.env_remove("CULPRIT_DELIVERY_CRASH_POINT");
            }
            let output = child.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(73),
                "child did not reach its durable boundary: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if root.path().join("crash-reached").exists() && !reached {
                reached = true;
                if point == "after-commit" {
                    std::fs::remove_dir_all(&prepared.directory).unwrap();
                }
            }
            let journal = DeliveryJournal::open(&root.path().join("delivery"), contract()).unwrap();
            if journal.status(&id).await.unwrap().phase == DeliveryPhase::Retired {
                retired = true;
                break;
            }
        }
        assert!(reached && retired, "{point} did not recover to retirement");
        {
            let store = Arc::new(FileRecoveryStore::open(&root.path().join("recovery"), contract()).unwrap());
            let range = reopen(store.as_ref(), &id).await.unwrap();
            assert_eq!(range.manifest(), prepared.manifest());
            let sink = FileTelemetrySink::open(&root.path().join("warehouse"), contract(), store).unwrap();
            assert_eq!(
                sink.physical_rows(&id).await.unwrap(),
                1,
                "same attempt must stay idempotent after an unknown outcome"
            );
            let verified = verify(&sink, &range, VerificationBudget::default()).await.unwrap();
            let publication = Publication { range_id: id.clone(), manifest_digest: range.manifest_digest().into() };
            assert!(sink.publication_visible(&publication, verified.committed()).await.unwrap());
        }
    }
}
#[tokio::test]
async fn notification_cursor_and_expiry_reconciliation_are_atomic_and_bounded() {
    let root = tempfile::TempDir::new().unwrap();
    let first = prepared(root.path(), 1);
    let second = prepared(root.path(), 2);
    let journal = DeliveryJournal::create(&root.path().join("delivery"), contract()).unwrap();
    journal.register(&first).await.unwrap();
    journal.register(&second).await.unwrap();
    let bad = DeliveryUpdates {
        updates: vec![
            DeliveryUpdate::Retry { range_id: first.manifest().range_id.clone() },
            DeliveryUpdate::Retry { range_id: "invalid".into() },
        ],
        next_cursor: "must-not-commit".into(),
        expired: false,
    };
    assert_eq!(problem(journal.apply_updates(bad).await.err().unwrap()), DeliveryProblem::Integrity);
    assert!(journal.poll_cursor().await.unwrap().is_none());
    assert!(!journal.load(&first.manifest().range_id).await.unwrap().state.reconcile);
    journal
        .apply_updates(DeliveryUpdates { updates: vec![], next_cursor: "fresh".into(), expired: true })
        .await
        .unwrap();
    assert_eq!(journal.reconcile_expired(1).await.unwrap(), 1);
    drop(journal);
    let journal = DeliveryJournal::open(&root.path().join("delivery"), contract()).unwrap();
    assert_eq!(journal.poll_cursor().await.unwrap().as_deref(), Some("fresh"));
    assert!(journal.load(&first.manifest().range_id).await.unwrap().state.reconcile);
    assert!(!journal.load(&second.manifest().range_id).await.unwrap().state.reconcile);
    assert_eq!(journal.reconcile_expired(1).await.unwrap(), 1);
    assert!(journal.load(&second.manifest().range_id).await.unwrap().state.reconcile);
    assert_eq!(journal.reconcile_expired(1).await.unwrap(), 0);
}

struct DelayedSink<'a> {
    inner: &'a FileTelemetrySink,
    skip_dispatch: std::sync::atomic::AtomicBool,
    stall_receipt: std::sync::atomic::AtomicBool,
    attempts: std::sync::Mutex<Vec<String>>,
}
#[async_trait]
impl TelemetrySink for DelayedSink<'_> {
    async fn validate_contract(&self, contract: &super::contract::Contract) -> Result<()> {
        self.inner.validate_contract(contract).await
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission> {
        self.attempts.lock().unwrap().push(attempt.id.clone());
        if self.skip_dispatch.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Ok(Submission::Unknown);
        }
        self.inner.dispatch(range, attempt, batch_id).await
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, rows: u32) -> Result<ReceiptPage> {
        if self.stall_receipt.swap(false, std::sync::atomic::Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.inner.receipts(range, cursor, rows).await
    }
    async fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        self.inner.publish(range, attempt).await
    }
    async fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        self.inner.publication_visible(publication, range).await
    }
    async fn poll_updates(&self, cursor: Option<&str>, limit: u32) -> Result<DeliveryUpdates> {
        self.inner.poll_updates(cursor, limit).await
    }
}
#[tokio::test]
async fn expired_notification_redrives_a_new_physical_attempt_with_identical_logical_evidence() {
    let root = tempfile::TempDir::new().unwrap();
    let prepared = prepared(root.path(), 1);
    let id = prepared.manifest().range_id.clone();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let file = FileTelemetrySink::create(&root.path().join("warehouse"), contract(), store.clone()).unwrap();
    let sink = DelayedSink {
        inner: &file,
        skip_dispatch: true.into(),
        stall_receipt: false.into(),
        attempts: Default::default(),
    };
    let mut journal = DeliveryJournal::create(&root.path().join("delivery"), contract()).unwrap();
    journal.register(&prepared).await.unwrap();
    let mut capabilities = crate::delivery::committed_cache::CommittedRangeCache::default();
    let mut driver = DeliveryDriver::new(
        &mut journal,
        store.as_ref(),
        &sink,
        &mut capabilities,
        DeliveryPolicy { visibility_delay_ms: 1, ..Default::default() },
    )
    .unwrap();
    let mut retired = false;
    for tick in 0..20 {
        let status = driver.step(&id, owner.fence(), tick * 1000).await.unwrap();
        if status.phase == DeliveryPhase::Retired {
            retired = true;
            break;
        }
    }
    assert!(retired);
    let attempts = sink.attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), 2);
    assert_ne!(attempts[0], attempts[1]);
    assert_eq!(file.physical_rows(&id).await.unwrap(), 1);
    let committed = reopen(store.as_ref(), &id).await.unwrap();
    assert_eq!(committed.manifest(), prepared.manifest());
}
#[tokio::test(start_paused = true)]
async fn receipt_timeout_preserves_the_submitted_attempt_and_recovers_without_resubmission() {
    let root = tempfile::TempDir::new().unwrap();
    let prepared = prepared(root.path(), 1);
    let id = prepared.manifest().range_id.clone();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract()).unwrap());
    let owner = store.acquire().await.unwrap();
    let file = FileTelemetrySink::create(&root.path().join("warehouse"), contract(), store.clone()).unwrap();
    let sink = DelayedSink {
        inner: &file,
        skip_dispatch: false.into(),
        stall_receipt: true.into(),
        attempts: Default::default(),
    };
    let mut journal = DeliveryJournal::create(&root.path().join("delivery"), contract()).unwrap();
    journal.register(&prepared).await.unwrap();
    let policy = DeliveryPolicy { visibility_delay_ms: 1, request_timeout_ms: 1, ..Default::default() };
    let mut now = 0;
    {
        let mut capabilities = crate::delivery::committed_cache::CommittedRangeCache::default();
        let mut driver = DeliveryDriver::new(&mut journal, store.as_ref(), &sink, &mut capabilities, policy).unwrap();
        let mut awaiting = false;
        for _ in 0..16 {
            let state = driver.step(&id, owner.fence(), now).await.unwrap();
            now += 1000;
            if state.phase == DeliveryPhase::AwaitingData {
                awaiting = true;
                break;
            }
        }
        assert!(awaiting);
        assert_eq!(problem(driver.step(&id, owner.fence(), now).await.err().unwrap()), DeliveryProblem::Transport);
    }
    let status = journal.status(&id).await.unwrap();
    assert_eq!(status.phase, DeliveryPhase::AwaitingData);
    assert_eq!(status.attempt, 1);
    let mut capabilities = crate::delivery::committed_cache::CommittedRangeCache::default();
    let mut driver = DeliveryDriver::new(&mut journal, store.as_ref(), &sink, &mut capabilities, policy).unwrap();
    let status = driver.step(&id, owner.fence(), now + 10_000).await.unwrap();
    assert_eq!(status.phase, DeliveryPhase::PublicationPending);
    assert_eq!(sink.attempts.lock().unwrap().len(), 1);
}
