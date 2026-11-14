use super::*;
use crate::delivery::{
    contract::Contract,
    diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_input},
    evidence::{PreparedRange, RangeManifest},
    file_store::{FileOwnership, FileRecoveryStore},
    recovery::*,
    sink::PublishedRange,
};
use crate::outbox::DiagnosticInputState;
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
                store.as_ref(),
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
    metadata: AtomicU32,
    opened: AtomicU32,
    active: AtomicU32,
}
struct RecoveryProbe {
    store: Arc<FileRecoveryStore>,
    object: String,
    reads: Arc<Reads>,
    advertised: Option<RangeManifest>,
    stall_at_eof: bool,
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
impl RecoveryStore for RecoveryProbe {
    fn id(&self) -> &str {
        self.store.id()
    }
    async fn capture_manifest(&self, capture: &CaptureIdentity) -> Result<RangeManifest> {
        self.reads.metadata.fetch_add(1, Ordering::SeqCst);
        match &self.advertised {
            Some(manifest) => {
                assert_eq!(&manifest.capture, capture);
                Ok(manifest.clone())
            }
            None => self.store.capture_manifest(capture).await,
        }
    }
    async fn load(&self, range_id: &str) -> Result<RangeManifest> {
        self.reads.metadata.fetch_add(1, Ordering::SeqCst);
        self.store.load(range_id).await
    }
    async fn open_object(&self, batch: &PayloadObject) -> Result<RecoveryReader> {
        assert_eq!(batch.object, self.object, "hydrate only the exact archived parent payload");
        let input = self.store.open_object(batch).await?;
        self.reads.opened.fetch_add(1, Ordering::SeqCst);
        if !self.stall_at_eof {
            return Ok(input);
        }
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
    async fn diagnostic_input_pending(&self, input: &DiagnosticCaptureRef, version: u32) -> Result<bool> {
        self.store.diagnostic_input_pending(input, version).await
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
    let recovery = RecoveryProbe {
        store: cold.store.clone(),
        object: cold.payload_object.clone(),
        reads: reads.clone(),
        advertised: None,
        stall_at_eof: true,
    };
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
    let recovery = RecoveryProbe {
        store: cold.store.clone(),
        object: cold.payload_object.clone(),
        reads: reads.clone(),
        advertised: None,
        stall_at_eof: true,
    };
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

struct ArchivedCost {
    harness: Harness,
    store: Arc<FileRecoveryStore>,
    committed: CommittedRange,
    reference: DiagnosticEvidenceRef,
    original: (CollectionDescription, ExportedRecord),
    archive: tempfile::TempDir,
    _owner: FileOwnership,
}
impl ArchivedCost {
    async fn new() -> Self {
        let archive = tempfile::tempdir().unwrap();
        let contract = Contract::new(
            "pipeline".into(),
            "shard".into(),
            "file:warehouse".into(),
            digest(b"qualification-artifact-schema"),
        )
        .unwrap();
        let store = Arc::new(FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap());
        let owner = store.acquire().await.unwrap();
        let proof = crate::analytical::test_support::investigation_fixture();
        let first = proof.record.record;
        let mut second = first.clone();
        let TelemetryRecord::Operational(observation) = &mut second else { panic!() };
        let OperationalFact::CounterSet(counters) = &mut observation.fact else { panic!() };
        counters.identity.id = "second-native-cost-observation".into();
        observation.key = counters.key().unwrap();
        let mut source = proof.source;
        let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
        snapshot.records = 2;
        snapshot.sources = vec![StateSourceCoverage::collected(source.source.clone(), 2)];
        let frame = Frame { source, records: vec![first, second] };
        let mut harness = Harness::with_recovery_store(Some(store.id())).await;
        harness.outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
        let capture = harness.capture(frame.clone()).await;
        let committed = archive_capture(&harness.outbox, store.as_ref(), owner.fence(), &contract, &capture).await;
        let row = ExportedRecord::new(&frame.source, &capture, frame.records[0].clone()).unwrap();
        let reference =
            DiagnosticEvidenceRef::from_export(&frame.source, &committed.manifest().capture_manifest_sha256, &row, 0)
                .unwrap();
        let original = (frame.source, row);
        Self { harness, store, committed, reference, original, archive, _owner: owner }
    }
    async fn evict(&self) {
        let delivery = self.harness.outbox.delivery();
        delivery.mark_archived(&self.committed).await.unwrap();
        assert!(delivery.evict(&self.committed).await.unwrap());
        assert_missing_parent(&self.harness.outbox, &self.reference).await;
    }
    fn payload_path(&self) -> PathBuf {
        self.archive.path().join("recovery").join(&self.committed.manifest().batches[0].object)
    }
}

#[tokio::test]
async fn a_local_qualification_artifact_is_authoritative_and_archived_operational_evidence_stays_transient() {
    let mut artifact = ArchivedCost::new().await;
    assert_eq!(
        resolve_evidence(
            &mut artifact.harness.outbox,
            "fixture",
            &artifact.reference,
            None,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap(),
        Some(artifact.original.clone())
    );
    let calls = artifact.harness.calls.counts();
    artifact.evict().await;
    let resolved = resolve_evidence(
        &mut artifact.harness.outbox,
        "fixture",
        &artifact.reference,
        Some(artifact.store.as_ref()),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(resolved, Some(artifact.original));
    assert_missing_parent(&artifact.harness.outbox, &artifact.reference).await;
    assert!(artifact.harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    assert_eq!(artifact.harness.calls.counts(), calls, "exact evidence resolution opens no source");
    assert_eq!(
        artifact
            .harness
            .outbox
            .diagnostic_input_state(&artifact.reference.capture_ref(), DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap(),
        DiagnosticInputState::Missing,
    );
}

#[tokio::test]
async fn a_nonzero_receipt_position_resolves_the_exact_native_record_in_its_committed_batch() {
    let mut artifact = ArchivedCost::new().await;
    let bytes = std::fs::read(artifact.payload_path()).unwrap();
    let row: ExportedRecord = serde_json::from_slice(bytes.split(|byte| *byte == b'\n').nth(1).unwrap()).unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&artifact.original.0, &artifact.reference.manifest_sha256, &row, 1).unwrap();
    assert_ne!(reference.record_id, artifact.reference.record_id);
    artifact.evict().await;
    assert_eq!(
        resolve_evidence(
            &mut artifact.harness.outbox,
            "fixture",
            &reference,
            Some(artifact.store.as_ref()),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap(),
        Some((artifact.original.0, row))
    );
}

#[tokio::test]
async fn a_recovered_activity_parent_can_be_pinned_without_becoming_consumed_or_requeued_work() {
    let mut cold = ColdCapture::new().await;
    let pending = cold.harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap();
    let resolved = resolve_evidence(
        &mut cold.harness.outbox,
        "fixture",
        &cold.focus.parent,
        Some(cold.store.as_ref()),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(resolved, Some(cold.original.clone()));
    assert_eq!(
        cold.harness
            .outbox
            .diagnostic_input_state(&cold.focus.parent.capture_ref(), DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap(),
        DiagnosticInputState::EvidenceOnly,
        "evidence-only acknowledgement prevents enqueueing; it does not prove committed analysis"
    );
    assert_eq!(
        cold.harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap().parent,
        pending.parent
    );
    assert_eq!(cold.harness.outbox.status(10, None).await.unwrap().captures, 0);
    let intent = digest(b"newly-validated-question-for-recovered-original");
    cold.harness.outbox.pin_investigation_parent(&intent, &cold.focus).await.unwrap();
    assert_eq!(
        cold.harness.outbox.investigation_parent_keys().await.unwrap().as_slice(),
        std::slice::from_ref(&intent)
    );
    cold.harness.outbox.release_investigation_parent(&intent).await.unwrap();
    assert_eq!(cold.harness.calls.counts(), [2, 2, 2, 2]);
}

#[tokio::test]
async fn wrong_manifest_position_hash_or_native_identity_cannot_hydrate_an_archived_parent() {
    let mut cold = ColdCapture::new().await;
    for mismatch in ["manifest", "ordinal", "payload", "record", "scope", "native"] {
        let mut reference = cold.focus.parent.clone();
        match mismatch {
            "manifest" => reference.manifest_sha256 = digest(b"another original manifest"),
            "ordinal" => reference.row_ordinal += 1,
            "payload" => reference.payload_sha256 = digest(b"another original row"),
            "record" => reference.record_id = digest(b"another record identity"),
            "scope" => reference.source_scope_id = digest(b"another native scope"),
            _ => {
                let NativeRecordIdentity::Activity(key) = &mut reference.native_key else { panic!() };
                key.session.id = "another-native-session".into();
            }
        }
        let error = resolve_evidence(
            &mut cold.harness.outbox,
            "fixture",
            &reference,
            Some(cold.store.as_ref()),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{mismatch}");
        cold.assert_untouched().await;
    }
}

#[tokio::test]
async fn archived_payload_truncation_is_rejected_even_when_the_selected_row_was_already_decoded() {
    let mut artifact = ArchivedCost::new().await;
    artifact.evict().await;
    let path = artifact.payload_path();
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'), "remove only final framing, leaving valid selected-row JSON");
    std::fs::write(path, bytes).unwrap();
    let error = resolve_evidence(
        &mut artifact.harness.outbox,
        "fixture",
        &artifact.reference,
        Some(artifact.store.as_ref()),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_missing_parent(&artifact.harness.outbox, &artifact.reference).await;
    assert!(artifact.harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
}

#[tokio::test]
async fn cancellation_at_an_operational_artifacts_eof_withholds_the_row_without_creating_cache_or_work() {
    let artifact = ArchivedCost::new().await;
    artifact.evict().await;
    let reads = Arc::new(Reads::default());
    let recovery = RecoveryProbe {
        store: artifact.store.clone(),
        object: artifact.committed.manifest().batches[0].object.clone(),
        reads: reads.clone(),
        advertised: None,
        stall_at_eof: true,
    };
    let cancellation = CollectionCancellation::default();
    let stop = cancellation.clone();
    let processing = tokio::spawn(async move {
        let mut artifact = artifact;
        let result =
            resolve_evidence(&mut artifact.harness.outbox, "fixture", &artifact.reference, Some(&recovery), &stop)
                .await;
        (artifact, result)
    });
    reach_stalled_eof(&reads).await;
    cancellation.cancel();
    let (artifact, result) = tokio::time::timeout(Duration::from_secs(1), processing).await.unwrap().unwrap();
    assert!(result.unwrap().is_none());
    assert_eq!(reads.active.load(Ordering::SeqCst), 0);
    assert_missing_parent(&artifact.harness.outbox, &artifact.reference).await;
    assert!(artifact.harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
}

#[tokio::test]
async fn a_corrupt_local_evidence_cache_is_never_hidden_by_an_archive_read() {
    let mut cold = ColdCapture::new().await;
    resolve_evidence(
        &mut cold.harness.outbox,
        "fixture",
        &cold.focus.parent,
        Some(cold.store.as_ref()),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let connection = rusqlite::Connection::open(cold.harness.path.join("journal.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE diagnostic_inputs SET restored_sha=?1 WHERE observation_id=?2",
            rusqlite::params![digest(b"altered cached evidence"), cold.focus.parent.capture.observation_id],
        )
        .unwrap();
    let reads = Arc::new(Reads::default());
    let recovery = RecoveryProbe {
        store: cold.store.clone(),
        object: cold.payload_object.clone(),
        reads: reads.clone(),
        advertised: None,
        stall_at_eof: true,
    };
    let error = resolve_evidence(
        &mut cold.harness.outbox,
        "fixture",
        &cold.focus.parent,
        Some(&recovery),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::Integrity)));
    assert_eq!(reads.metadata.load(Ordering::SeqCst), 0);
    assert_eq!(reads.opened.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn archive_identity_and_advertised_manifest_changes_cannot_replace_the_exact_parent() {
    let mut cold = ColdCapture::new().await;
    let other_directory = tempfile::tempdir().unwrap();
    let other = FileRecoveryStore::create(
        &other_directory.path().join("different-store"),
        cold.store.capture_manifest(&cold.focus.parent.capture).await.unwrap().contract,
    )
    .unwrap();
    let error = resolve_evidence(
        &mut cold.harness.outbox,
        "fixture",
        &cold.focus.parent,
        Some(&other),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::DestinationChanged)));
    cold.assert_untouched().await;

    let mut changed = cold.store.capture_manifest(&cold.focus.parent.capture).await.unwrap();
    changed.collected_from = "2001-01-01T00:00:00Z".into();
    changed.validate().unwrap();
    let reads = Arc::new(Reads::default());
    let recovery = RecoveryProbe {
        store: cold.store.clone(),
        object: cold.payload_object.clone(),
        reads,
        advertised: Some(changed),
        stall_at_eof: false,
    };
    let error = resolve_evidence(
        &mut cold.harness.outbox,
        "fixture",
        &cold.focus.parent,
        Some(&recovery),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    cold.assert_untouched().await;
}
