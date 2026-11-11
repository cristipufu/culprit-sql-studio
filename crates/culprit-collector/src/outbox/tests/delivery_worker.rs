use super::*;
use crate::delivery::{
    contract::Contract,
    driver::DeliveryPolicy,
    file_sink::FileTelemetrySink,
    file_store::FileRecoveryStore,
    journal::DeliveryJournal,
    recovery::*,
    sink::*,
    worker::{DeliveryJournals, DeliveryWorker},
};
use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct OfflineSink {
    available: AtomicBool,
    inner: FileTelemetrySink,
}
struct SlowReadback {
    inner: FileTelemetrySink,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    pending: AtomicUsize,
}
struct ReadbackInFlight<'a>(&'a AtomicUsize);
impl Drop for ReadbackInFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl TelemetrySink for SlowReadback {
    async fn validate_contract(&self, contract: &Contract) -> Result<()> {
        self.inner.validate_contract(contract).await
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission> {
        self.inner.dispatch(range, attempt, batch_id).await
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let _in_flight = ReadbackInFlight(&self.pending);
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.receipts(range, cursor, page_rows).await
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
struct Quiet;
impl crate::service::ServiceReporter for Quiet {
    fn collection(&self, _: &FleetCollectionEvent) -> Result<()> {
        unreachable!()
    }
    fn source_paused(&self, _: &culprit_core::Error) -> Result<()> {
        unreachable!()
    }
    fn delivery(&self, _: Option<&DeliveryEntry>, _: Result<DeliveryEntry>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_stalled_analytical_readback_does_not_block_new_native_archive_commits() {
    use crate::{capture::CollectionCancellation, service::run_delivery};
    use std::time::Duration;
    // This proves ownership independence, not a five-second filesystem throughput budget on every runner.
    const PROGRESS_WATCHDOG: Duration = Duration::from_secs(60);
    let root = tempfile::tempdir().unwrap();
    let contract = Contract::new("pipeline".into(), "shard-0".into(), "file:test".into(), digest(b"schema")).unwrap();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap());
    let owner = store.acquire().await.unwrap();
    let mut outbox = Outbox::create_remote(&root.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    complete(&mut outbox).await;
    let sink = SlowReadback {
        inner: FileTelemetrySink::create(&root.path().join("warehouse"), contract.clone(), store.clone()).unwrap(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        pending: AtomicUsize::new(0),
    };
    let mut journal = DeliveryJournal::create(&root.path().join("delivery"), contract.clone()).unwrap();
    let mut archive = DeliveryJournal::create(&root.path().join("archive"), contract.clone()).unwrap();
    let mut worker = DeliveryWorker::new(
        outbox.delivery(),
        DeliveryJournals { archive: &mut archive, analytical: &mut journal },
        &*store,
        &sink,
        owner.fence().clone(),
        contract.clone(),
        DeliveryPolicy { visibility_delay_ms: 1, request_timeout_ms: 300_000, ..Default::default() },
    )
    .unwrap();
    let cancellation = CollectionCancellation::default();
    let running = run_delivery(&mut worker, &cancellation, &Quiet);
    tokio::pin!(running);
    tokio::select! {
        result=&mut running=>panic!("delivery ended before the stalled query: {result:?}"),
        result=tokio::time::timeout(PROGRESS_WATCHDOG,sink.entered.notified())=>result.unwrap(),
    }
    let second = complete(&mut outbox).await;
    let prepared = outbox.delivery().prepare(&second.capture.observation_id, contract, vec![]).await.unwrap();
    let range_id = prepared.manifest().range_id.clone();
    let probe = async {
        loop {
            match store.load(&range_id).await {
                Ok(manifest) => return manifest == *prepared.manifest(),
                Err(error) if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady)) => {}
                Err(error) => panic!("unexpected native recovery error: {error:?}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let archived = tokio::select! {
        result=&mut running=>panic!("delivery ended while a destination query was stalled: {result:?}"),
        result=tokio::time::timeout(PROGRESS_WATCHDOG,probe)=>result.unwrap_or(false),
    };
    let still_blocked = sink.pending.load(Ordering::SeqCst) == 1;
    cancellation.cancel();
    sink.release.notify_waiters();
    tokio::time::timeout(PROGRESS_WATCHDOG, running).await.unwrap().unwrap();
    assert!(archived, "a stalled destination query must not hold the owner that commits new native captures");
    assert!(still_blocked, "native archival must complete while the same destination readback is still pending");
}
#[async_trait]
impl TelemetrySink for OfflineSink {
    async fn validate_contract(&self, contract: &Contract) -> Result<()> {
        if !self.available.load(Ordering::Acquire) {
            return Err(DeliveryProblem::Transport.error());
        }
        self.inner.validate_contract(contract).await
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission> {
        self.inner.dispatch(range, attempt, batch_id).await
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, rows: u32) -> Result<ReceiptPage> {
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
async fn destination_outage_does_not_prevent_remote_commit_eviction_or_more_source_capture() {
    let root = tempfile::TempDir::new().unwrap();
    let contract = Contract::new("pipeline".into(), "shard-0".into(), "file:test".into(), digest(b"schema")).unwrap();
    let store = Arc::new(FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap());
    let owner = store.acquire().await.unwrap();
    let mut outbox = Outbox::create_remote(
        &root.path().join("outbox"),
        OutboxLimits { max_captures: 1, ..Default::default() },
        store.id(),
    )
    .unwrap();
    outbox.recover().await.unwrap();
    outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    let first = complete(&mut outbox).await;
    let first_id = digest(&serde_json::to_vec(&(&contract, &first.capture)).unwrap());
    let sink = OfflineSink {
        available: AtomicBool::new(false),
        inner: FileTelemetrySink::create(&root.path().join("warehouse"), contract.clone(), store.clone()).unwrap(),
    };
    let mut journal = DeliveryJournal::create(&root.path().join("delivery"), contract.clone()).unwrap();
    let mut archive = DeliveryJournal::create(&root.path().join("archive"), contract.clone()).unwrap();
    let health = journal.health_reader();
    let policy = DeliveryPolicy { visibility_delay_ms: 1, retry_delay_ms: 1, ..Default::default() };
    let mut worker = DeliveryWorker::new(
        outbox.delivery(),
        DeliveryJournals { archive: &mut archive, analytical: &mut journal },
        &*store,
        &sink,
        owner.fence().clone(),
        contract,
        policy,
    )
    .unwrap();
    let mut now = 0;
    for _ in 0..12 {
        worker
            .work_once(now, &crate::capture::CollectionCancellation::default(), |_, result| {
                if let Err(error) = result {
                    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Transport)));
                }
                Ok(())
            })
            .await
            .unwrap();
        now += 1;
        if outbox.status(50, None).await.unwrap().captures == 0 {
            break;
        }
    }
    assert_eq!(outbox.status(50, None).await.unwrap().captures, 0);
    let backlog = health.backlog().await.unwrap();
    assert_eq!(backlog.pending, 1);
    assert_eq!(
        backlog.oldest_pending_collection_ms,
        Some(chrono::DateTime::parse_from_rfc3339(first.finished_at.as_ref().unwrap()).unwrap().timestamp_millis())
    );
    let second = outbox.reserve("fixture", &request()).await.unwrap();
    let mut errors = 0;
    worker
        .work_once(now, &crate::capture::CollectionCancellation::default(), |_, result| {
            assert_eq!(
                result.err().unwrap().detail,
                Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Transport))
            );
            errors += 1;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(errors, 1);
    let mut second = second;
    sealed(&mut second).await;
    second.finish().await.unwrap();
    for _ in 0..12 {
        now += 1;
        worker.work_once(now, &crate::capture::CollectionCancellation::default(), |_, _| Ok(())).await.unwrap();
        if outbox.status(50, None).await.unwrap().captures == 0 {
            break;
        }
    }
    assert_eq!(outbox.status(50, None).await.unwrap().captures, 0);
    assert_eq!(store.recover(None, 50).await.unwrap().ranges.len(), 2);
    sink.available.store(true, Ordering::Release);
    for _ in 0..60 {
        now += 1;
        worker
            .work_once(now, &crate::capture::CollectionCancellation::default(), |_, result| {
                result.unwrap();
                Ok(())
            })
            .await
            .unwrap();
    }
    drop(worker);
    assert!(journal.pending(0, 50).await.unwrap().0.is_empty());
    assert_eq!(health.backlog().await.unwrap(), DeliveryBacklog::default());
    let error = journal.status(&first_id).await.err().unwrap();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady)));
    assert_eq!(
        sink.inner.physical_rows(&first_id).await.unwrap(),
        3,
        "one runtime row and its query and plan definitions"
    );
    assert_eq!(outbox.status(50, None).await.unwrap().charged_bytes, 0);
}
