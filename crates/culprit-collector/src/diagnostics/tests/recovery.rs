use super::*;
use crate::delivery::{
    contract::Contract,
    diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_input},
    evidence::{PreparedRange, RangeManifest},
    file_store::{FileOwnership, FileRecoveryStore},
    recovery::*,
    sink::PublishedRange,
};
use culprit_core::{ErrorCode, ErrorDetail};
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, ReadBuf};

struct ColdCapture {
    harness: Harness,
    store: Arc<FileRecoveryStore>,
    focus: ActivityWaitFocus,
    input: CaptureIdentity,
    original: (CollectionDescription, ExportedRecord),
    checkpoint: DiagnosticCheckpoint,
    payload_object: String,
    _owner: FileOwnership,
    _archive: tempfile::TempDir,
}
impl ColdCapture {
    async fn new() -> Self {
        let archive = tempfile::tempdir().unwrap();
        let contract = Contract::new(
            "pipeline".into(),
            "shard".into(),
            "file:warehouse".into(),
            digest(b"diagnostic-recovery-schema"),
        )
        .unwrap();
        let store = Arc::new(FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap());
        let owner = store.acquire().await.unwrap();
        let mut hot = Harness::with_recovery_store(Some(store.id())).await;
        hot.outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
        let parent = hot.capture(baseline(0, vec![participant()])).await;
        assert_eq!(hot.work(1).await, 1);
        let focus = hot.focus(&parent).await;
        let original = hot.outbox.diagnostic_evidence(&focus.parent).await.unwrap();
        let native = archive_capture(&hot.outbox, store.as_ref(), owner.fence(), &contract, &parent).await;
        let payload_object = native.manifest().batches[0].object.clone();
        let derived = hot
            .outbox
            .status(10, None)
            .await
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| matches!(entry.request.selection, CollectionSelection::Diagnostics { .. }))
            .unwrap()
            .capture;
        archive_capture(&hot.outbox, store.as_ref(), owner.fence(), &contract, &derived).await;
        let input = hot.capture(missing(focus.clone(), 10)).await;
        let focused = archive_capture(&hot.outbox, store.as_ref(), owner.fence(), &contract, &input).await;
        let pending = store.pending_diagnostic_inputs(None, 10).await.unwrap();
        assert_eq!(pending.ranges.len(), 1, "the original parent has already been analyzed");
        assert_eq!(pending.ranges[0].capture, input);
        let head = store.diagnostic_heads(None, 10).await.unwrap();
        assert_eq!(head.heads.len(), 1);
        let recovered = load_diagnostic_checkpoint(store.as_ref(), &head.heads[0]).await.unwrap();
        let checkpoint = recovered.checkpoint().clone();
        let calls = hot.calls.clone();
        let Harness { outbox, root, .. } = hot;
        drop(outbox);
        drop(root);

        let mut harness = Harness::with_recovery_store(Some(store.id())).await;
        harness.calls = calls;
        harness.outbox.restore_diagnostics(recovered).await.unwrap();
        harness
            .outbox
            .restore_diagnostic_input(
                "fixture",
                load_diagnostic_input(store.as_ref(), &focused.manifest().range_id).await.unwrap(),
            )
            .await
            .unwrap();
        harness.outbox.begin_diagnostic_epoch(owner.fence(), pending.high_sequence).await.unwrap();
        assert_eq!(harness.checkpoint().await, checkpoint);
        assert_missing_parent(&harness.outbox, &focus.parent).await;
        Self { harness, store, focus, input, original, checkpoint, payload_object, _owner: owner, _archive: archive }
    }
    async fn assert_untouched(&self) {
        assert_missing_parent(&self.harness.outbox, &self.focus.parent).await;
        assert_eq!(self.harness.checkpoint().await, self.checkpoint);
        let pending = self.harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap();
        assert_eq!(pending.parent.capture, self.input);
        let status = self.harness.outbox.status(10, None).await.unwrap();
        assert_eq!((status.captures, status.source_debts, status.unfinished), (0, 0, 0));
        assert_eq!(self.harness.calls.counts(), [2, 2, 2, 2]);
    }
}
async fn archive_capture(
    outbox: &Outbox,
    store: &dyn RecoveryStore,
    fence: &Fence,
    contract: &Contract,
    capture: &CaptureIdentity,
) -> CommittedRange {
    let prepared: PreparedRange =
        outbox.delivery().prepare(&capture.observation_id, contract.clone(), vec![]).await.unwrap();
    let head = store.checkpoint_head().await.unwrap();
    stage_and_commit(store, fence, head.as_deref(), &prepared).await.unwrap()
}
async fn assert_missing_parent(outbox: &Outbox, parent: &DiagnosticEvidenceRef) {
    assert_eq!(
        outbox.diagnostic_evidence(parent).await.unwrap_err().detail,
        Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture))
    );
}

#[derive(Default)]
struct Reads {
    eof: tokio::sync::Notify,
    opened: AtomicU32,
    active: AtomicU32,
}
struct StalledRecovery {
    store: Arc<FileRecoveryStore>,
    object: String,
    reads: Arc<Reads>,
}
struct EofStall {
    input: RecoveryReader,
    reads: Arc<Reads>,
    stalled: bool,
}
impl AsyncRead for EofStall {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.stalled {
            return Poll::Pending;
        }
        let before = output.filled().len();
        match self.input.as_mut().poll_read(cx, output) {
            Poll::Ready(Ok(())) if output.filled().len() == before => {
                self.stalled = true;
                self.reads.eof.notify_one();
                Poll::Pending
            }
            result => result,
        }
    }
}
impl Drop for EofStall {
    fn drop(&mut self) {
        self.reads.active.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl RecoveryStore for StalledRecovery {
    fn id(&self) -> &str {
        self.store.id()
    }
    async fn capture_manifest(&self, capture: &CaptureIdentity) -> Result<RangeManifest> {
        self.store.capture_manifest(capture).await
    }
    async fn load(&self, range_id: &str) -> Result<RangeManifest> {
        self.store.load(range_id).await
    }
    async fn open_object(&self, batch: &PayloadObject) -> Result<RecoveryReader> {
        assert_eq!(batch.object, self.object, "hydrate only the exact archived parent payload");
        let input = self.store.open_object(batch).await?;
        self.reads.opened.fetch_add(1, Ordering::SeqCst);
        self.reads.active.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(EofStall { input, reads: self.reads.clone(), stalled: false }))
    }
    async fn checkpoint_head(&self) -> Result<Option<String>> {
        panic!("parent lookup does not enumerate history")
    }
    async fn captured_range(&self, _: &CapturedRange) -> Result<Option<CaptureIdentity>> {
        panic!("no native discovery")
    }
    async fn recover(&self, _: Option<&str>, _: u32) -> Result<RecoveryPage> {
        panic!("no unbounded history recovery")
    }
    async fn pending_ranges(&self, _: Option<&str>, _: u32, _: RecoveryOrder) -> Result<RecoveryPage> {
        panic!("no delivery enumeration")
    }
    async fn pending_diagnostic_inputs(&self, _: Option<&str>, _: u32) -> Result<RecoveryPage> {
        panic!("input is already selected")
    }
    async fn diagnostic_heads(&self, _: Option<&str>, _: u32) -> Result<DiagnosticHeadPage> {
        panic!("checkpoint is already restored")
    }
    async fn stage(&self, _: &PayloadObject, _: &Path) -> Result<StagedBatch> {
        panic!("analysis never stages recovery objects")
    }
    async fn commit(&self, _: &Fence, _: Option<&str>, _: &RangeManifest, _: &[StagedBatch]) -> Result<CommitReceipt> {
        panic!("analysis never commits remote recovery")
    }
    async fn retire(&self, _: &Fence, _: &PublishedRange) -> Result<()> {
        panic!("analysis never retires delivery")
    }
}

async fn reach_stalled_eof(reads: &Reads) {
    tokio::time::timeout(Duration::from_secs(5), reads.eof.notified())
        .await
        .expect("actual archived bytes reached their stalled EOF");
    assert_eq!(reads.opened.load(Ordering::SeqCst), 1);
    assert_eq!(reads.active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_archived_already_analyzed_parent_is_hydrated_exactly_without_requeueing_it_after_volume_loss() {
    let mut cold = ColdCapture::new().await;
    let calls = cold.harness.calls.counts();
    let processed = analyze_pending(
        &mut cold.harness.outbox,
        &cold.harness.policies,
        &CollectionCancellation::default(),
        1,
        Some(cold.store.as_ref()),
    )
    .await
    .unwrap();
    assert_eq!(processed, 1);
    assert_eq!(cold.harness.calls.counts(), calls);
    assert_eq!(cold.harness.outbox.diagnostic_evidence(&cold.focus.parent).await.unwrap(), cold.original);
    assert!(cold.harness.checkpoint().await.episodes.is_empty());
    let revisions = cold.harness.revisions(&cold.input).await;
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(revisions[0].episode.first, cold.focus.parent);
    assert!(
        cold.harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none(),
        "hydrating historical evidence must not rerun its already committed original analysis"
    );
    let path = cold.harness.path.clone();
    drop(cold.harness.outbox);
    let mut reopened = Outbox::open(&path).unwrap();
    assert_eq!(reopened.recover().await.unwrap().source_debts, 0);
    assert_eq!(reopened.diagnostic_evidence(&cold.focus.parent).await.unwrap(), cold.original);
    assert!(reopened.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
}

#[tokio::test]
async fn cancelling_a_cold_parent_read_at_eof_keeps_the_input_pending_and_never_caches_unverified_bytes() {
    let cold = ColdCapture::new().await;
    let reads = Arc::new(Reads::default());
    let recovery =
        StalledRecovery { store: cold.store.clone(), object: cold.payload_object.clone(), reads: reads.clone() };
    let cancellation = CollectionCancellation::default();
    let stop = cancellation.clone();
    let processing = tokio::spawn(async move {
        let mut cold = cold;
        let result = analyze_pending(&mut cold.harness.outbox, &cold.harness.policies, &stop, 1, Some(&recovery)).await;
        (cold, result)
    });
    reach_stalled_eof(&reads).await;
    cancellation.cancel();
    let (cold, result) = tokio::time::timeout(Duration::from_secs(1), processing).await.unwrap().unwrap();
    assert_eq!(result.unwrap(), 0);
    assert_eq!(reads.active.load(Ordering::SeqCst), 0, "cancellation drops its owned read transport");
    cold.assert_untouched().await;
}

#[tokio::test]
async fn the_cold_parent_read_deadline_preserves_pending_input_and_drops_its_owned_transport() {
    let cold = ColdCapture::new().await;
    let reads = Arc::new(Reads::default());
    let recovery =
        StalledRecovery { store: cold.store.clone(), object: cold.payload_object.clone(), reads: reads.clone() };
    let processing = tokio::spawn(async move {
        let mut cold = cold;
        let result = analyze_pending(
            &mut cold.harness.outbox,
            &cold.harness.policies,
            &CollectionCancellation::default(),
            1,
            Some(&recovery),
        )
        .await;
        (cold, result)
    });
    reach_stalled_eof(&reads).await;
    tokio::time::pause();
    tokio::time::advance(RECOVERY_READ_TIMEOUT).await;
    let (cold, result) = processing.await.unwrap();
    tokio::time::resume();
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Storage)));
    assert_eq!(reads.active.load(Ordering::SeqCst), 0, "deadline drops its owned read transport");
    cold.assert_untouched().await;
}
