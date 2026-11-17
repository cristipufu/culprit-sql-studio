//! Completed population receipts precede any recovery work, including after actual local eviction.
use super::*;
use crate::delivery::{
    contract::Contract, evidence::RangeManifest, file_store::FileRecoveryStore, recovery::*, sink::PublishedRange,
};
use culprit_core::{ErrorDetail, Result};
use std::{path::Path, sync::atomic::AtomicUsize};

struct ReadProbe<'a> {
    store: &'a FileRecoveryStore,
    manifests: AtomicUsize,
    objects: AtomicUsize,
}

impl ReadProbe<'_> {
    fn counts(&self) -> [usize; 2] {
        [self.manifests.load(Ordering::SeqCst), self.objects.load(Ordering::SeqCst)]
    }
}

#[async_trait::async_trait]
impl RecoveryStore for ReadProbe<'_> {
    fn id(&self) -> &str {
        self.store.id()
    }
    async fn capture_manifest(&self, capture: &CaptureIdentity) -> Result<RangeManifest> {
        self.manifests.fetch_add(1, Ordering::SeqCst);
        self.store.capture_manifest(capture).await
    }
    async fn load(&self, range_id: &str) -> Result<RangeManifest> {
        self.manifests.fetch_add(1, Ordering::SeqCst);
        self.store.load(range_id).await
    }
    async fn open_object(&self, object: &PayloadObject) -> Result<RecoveryReader> {
        self.objects.fetch_add(1, Ordering::SeqCst);
        self.store.open_object(object).await
    }
    async fn diagnostic_input_pending(&self, input: &DiagnosticCaptureRef, version: u32) -> Result<bool> {
        self.manifests.fetch_add(1, Ordering::SeqCst);
        self.store.diagnostic_input_pending(input, version).await
    }
    async fn checkpoint_head(&self) -> Result<Option<String>> {
        panic!("planning does not enumerate history")
    }
    async fn captured_range(&self, _: &CapturedRange) -> Result<Option<CaptureIdentity>> {
        panic!("no native discovery")
    }
    async fn recover(&self, _: Option<&str>, _: u32) -> Result<RecoveryPage> {
        panic!("no history sweep")
    }
    async fn pending_ranges(&self, _: Option<&str>, _: u32, _: RecoveryOrder) -> Result<RecoveryPage> {
        panic!("no delivery enumeration")
    }
    async fn pending_diagnostic_inputs(&self, _: Option<&str>, _: u32) -> Result<RecoveryPage> {
        panic!("checkpoint already selected")
    }
    async fn diagnostic_heads(&self, _: Option<&str>, _: u32) -> Result<DiagnosticHeadPage> {
        panic!("checkpoint already local")
    }
    async fn stage(&self, _: &PayloadObject, _: &Path) -> Result<StagedBatch> {
        panic!("planning is read only")
    }
    async fn commit(&self, _: &Fence, _: Option<&str>, _: &RangeManifest, _: &[StagedBatch]) -> Result<CommitReceipt> {
        panic!("planning is read only")
    }
    async fn retire(&self, _: &Fence, _: &PublishedRange) -> Result<()> {
        panic!("planning never retires evidence")
    }
}

#[tokio::test]
async fn completed_population_skips_evicted_original_before_any_archive_cost_or_source_work() {
    let archive = tempfile::tempdir().unwrap();
    let contract = Contract::new(
        "pipeline".into(),
        "one".into(),
        "file:population-stage".into(),
        digest(b"population-stage-schema"),
    )
    .unwrap();
    let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut fixture = Harness::with_archived_activity(activity(true, false), owner.fence()).await;
    configure_recipe(&mut fixture);
    let mut runtime = Runtime::from_fixture(fixture).await;
    assert!(runtime.enroll().await);
    let admitted = intent(&runtime).await;
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
        .await
        .unwrap();
    runtime.reconcile().await;
    assert!(matches!(
        runtime.fixture.journal.focused_stage(&admitted.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));

    let mut entries = runtime.fixture.outbox.status(100, None).await.unwrap().entries;
    entries.sort_by_key(|entry| entry.capture.revision.as_ref().unwrap().sequence.parse::<u64>().unwrap());
    let mut original = None;
    for entry in entries.into_iter().filter(|entry| entry.state == OutboxCaptureState::Ready) {
        let prepared = runtime
            .fixture
            .outbox
            .delivery()
            .prepare(&entry.capture.observation_id, contract.clone(), vec![])
            .await
            .unwrap();
        let head = store.checkpoint_head().await.unwrap();
        let committed = stage_and_commit(&store, owner.fence(), head.as_deref(), &prepared).await.unwrap();
        runtime.fixture.outbox.delivery().mark_archived(&committed).await.unwrap();
        if entry.capture == admitted.focus.participant().parent.capture {
            original = Some(committed);
        }
    }
    assert!(runtime.fixture.outbox.delivery().evict(&original.unwrap()).await.unwrap());
    assert_eq!(
        runtime.fixture.outbox.diagnostic_evidence(&admitted.focus.participant().parent).await.unwrap_err().detail,
        Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture))
    );
    let probe = ReadProbe { store: &store, manifests: AtomicUsize::new(0), objects: AtomicUsize::new(0) };
    let costs = runtime.fixture.backend.verified.load(Ordering::SeqCst);
    let calls = runtime.native_calls();
    for _ in 0..2 {
        let prepared = crate::fleet::investigations::planning::prepare(
            &runtime.fixture.backend,
            &runtime.fixture.journal,
            &mut runtime.fixture.outbox,
            Some(&probe),
            &runtime.fixture.inventory.targets,
            &runtime.fixture.bindings,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
        assert!(prepared.is_empty());
    }
    assert_eq!(probe.counts(), [0, 0], "completed first-frame stages require no manifest or object GET");
    assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), costs);
    assert_eq!(runtime.native_calls(), calls);
}

#[tokio::test]
async fn a_new_incident_first_capture_can_progress_after_the_earlier_population_stage_completed() {
    let mut runtime = Runtime::from_fixture(fixture(true, false).await).await;
    assert!(runtime.enroll().await);
    let original = intent(&runtime).await;
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
        .await
        .unwrap();
    runtime.reconcile().await;
    let source = &mut runtime.fixture.backend.activity.source;
    let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
    let at = chrono::DateTime::parse_from_rfc3339(&native.observed_until).unwrap() + chrono::Duration::seconds(20);
    native.observed_from = at.to_rfc3339();
    native.at = native.observed_from.clone();
    native.observed_until = (at + chrono::Duration::seconds(1)).to_rfc3339();
    for row in &mut runtime.fixture.backend.activity.records {
        let TelemetryRecord::Activity(row) = row else { panic!() };
        row.key.session.generation.push_str("-independent-incarnation");
    }
    runtime.started -= runtime.scheduler.wait_allowed(runtime.started.elapsed(), |_| false);
    runtime.baseline().await;
    let calls = runtime.native_calls();
    // Execution independently revalidated the first qualification. Only this new enrollment adds one check.
    let cost_checks = runtime.fixture.backend.verified.load(Ordering::SeqCst);
    assert!(runtime.enroll().await);
    let next = intent(&runtime).await;
    assert_ne!(next.focus.participant().parent.capture_ref(), original.focus.participant().parent.capture_ref());
    assert_ne!(next.episode_stage_key().unwrap(), original.episode_stage_key().unwrap());
    assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), cost_checks + 1);
    assert_eq!(runtime.native_calls(), calls, "enrolling the independent question does not dispatch it");
}
