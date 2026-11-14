use super::*;
use culprit_collector::{
    capture::{self, SourceFactory},
    delivery::{
        diagnostic_recovery::DiagnosticHeadPage,
        evidence::{PreparedRange, RangeManifest},
        file_store::FileRecoveryStore,
        recovery::*,
    },
    outbox::DiagnosticUpdate,
};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader, admission::SourceGate};
use std::{collections::VecDeque, sync::Mutex};

struct NativeFrame {
    source: CollectionDescription,
    rows: Vec<TelemetryRecord>,
}
#[async_trait::async_trait]
impl ObservationReader for NativeFrame {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<FrozenObservation> {
        Ok(FrozenObservation { description: self.source.clone(), records: self.rows.clone() })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        Ok(true)
    }
    fn query_count(&self) -> u32 {
        0
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        SourceShutdown::Quiescent
    }
}
#[async_trait::async_trait]
impl SourceFactory for NativeFrame {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(FrozenSource::new(Self { source: self.source.clone(), rows: self.rows.clone() }, request)?))
    }
}
async fn capture_frame(outbox: &mut Outbox, frame: NativeFrame) -> CollectionManifest {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: Default::default(),
    };
    let mut lease = outbox.reserve("removed-or-renamed-alias", &request).await.unwrap();
    let directory = lease.directory().to_owned();
    let identity = lease.identity().clone();
    let result = capture::collect(
        &SourceGate::default(),
        &frame,
        request,
        &directory,
        identity,
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await
    .unwrap();
    lease.finish().await.unwrap();
    assert_eq!(result.status, CollectionStatus::Complete);
    result
}
async fn archive(
    store: &FileRecoveryStore,
    fence: &Fence,
    outbox: &Outbox,
    capture: &CaptureIdentity,
    contract: &Contract,
) -> CommittedRange {
    let prepared: PreparedRange =
        outbox.delivery().prepare(&capture.observation_id, contract.clone(), vec![]).await.unwrap();
    let previous = store.checkpoint_head().await.unwrap();
    stage_and_commit(store, fence, previous.as_deref(), &prepared).await.unwrap()
}
fn inventory() -> FleetInventory {
    FleetInventory {
        targets: vec![FleetTarget {
            id: "configured-target".into(),
            connection: "different-current-alias".into(),
            subject: CollectionSubject::Database,
            shard: "alpha".into(),
            admission_group: "instance".into(),
            schedules: vec![FleetSchedule {
                id: "baseline".into(),
                every_seconds: 60,
                jitter_seconds: 0,
                collection: ScheduledCollection::Activity { policy: ActivityPolicy::default() },
                budget: Default::default(),
            }],
        }],
    }
}

#[tokio::test]
async fn cold_start_restores_full_checkpoint_then_only_owed_native_inputs_without_saved_route_or_credentials() {
    let root = tempfile::tempdir().unwrap();
    let contract = Contract::new("pipeline".into(), "alpha".into(), "file:test".into(), digest(b"schema")).unwrap();
    let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let original_path = root.path().join("lost-outbox");
    let mut original = Outbox::create_remote(&original_path, OutboxLimits::default(), store.id()).unwrap();
    original.recover().await.unwrap();
    original.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    let (source, rows) = culprit_collector::analytical::test_support::activity_evidence_cases().remove(0);
    let first = capture_frame(&mut original, NativeFrame { source: source.clone(), rows }).await;
    archive(&store, owner.fence(), &original, &first.capture, &contract).await;
    let input = original.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap();
    let facts = input
        .rows
        .iter()
        .map(|row| {
            let TelemetryRecord::Activity(observation) = &row.row.record else { panic!("native activity fixture") };
            DiagnosticActivityRow { reference: &row.reference, observation }
        })
        .collect::<Vec<_>>();
    let update = observe_diagnostic_activity(
        &input.source,
        &input.parent,
        &facts,
        None,
        &ActivityPolicy::default(),
        "2020-01-01T12:00:01Z",
    )
    .unwrap();
    assert!(update.checkpoint.episodes.len() >= 2, "restore the working set, not one latest episode");
    let checkpoint = update.checkpoint.clone();
    let derived = original
        .commit_diagnostics(
            input,
            DiagnosticUpdate {
                step_id: digest(b"first-analysis"),
                analyzed_at: "2020-01-01T12:00:01Z".into(),
                revisions: update.revisions,
                checkpoint: update.checkpoint,
            },
        )
        .await
        .unwrap();
    archive(&store, owner.fence(), &original, &derived, &contract).await;
    let mut empty = source.clone();
    let SourceObservation::Activity(frame) = &mut empty.observation else { panic!() };
    frame.observed_from = "2020-01-01T12:01:00Z".into();
    frame.observed_until = frame.observed_from.clone();
    frame.at = frame.observed_from.clone();
    frame.native_rows = 0;
    frame.observations = 0;
    frame.coverage = StateSourceCoverage::collected(empty.source.clone(), 0);
    let second = capture_frame(&mut original, NativeFrame { source: empty.clone(), rows: vec![] }).await;
    archive(&store, owner.fence(), &original, &second.capture, &contract).await;
    drop(original);
    drop(owner);
    std::fs::remove_dir_all(original_path).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut restored =
        Outbox::create_remote(&root.path().join("replacement"), OutboxLimits::default(), store.id()).unwrap();
    restored.recover().await.unwrap();
    let high = restore_diagnostic_state(&mut restored, &store, &CollectionCancellation::default()).await.unwrap();
    assert_eq!(high, second.capture.revision.as_ref().unwrap().sequence.parse::<u64>().unwrap());
    let restored_heads = restored.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 128).await.unwrap();
    assert_eq!(restored_heads.checkpoints, vec![checkpoint.clone()]);
    let pending = restored.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap();
    assert_eq!(pending.parent.capture, second.capture, "warehouse lag must not requeue already-analyzed first input");
    assert_eq!(pending.source, empty);
    assert_eq!(
        pending.connection,
        source.logical_source_id().unwrap(),
        "offline labels are not inferred native routes"
    );
    assert_ne!(pending.connection, "removed-or-renamed-alias");
    assert_eq!(pending.checkpoint, Some(checkpoint));
    assert!(pending.rows.is_empty(), "a genuine captured empty batch retains its original receipt");
    assert!(!root.path().join("workspace").exists());
    restored.begin_diagnostic_epoch(owner.fence(), high).await.unwrap();
    let policies = DiagnosticPolicies::from_inventory(&inventory(), "alpha").unwrap();
    drain_diagnostics(&mut restored, &policies, &CollectionCancellation::default(), Some(&store)).await.unwrap();
    assert!(restored.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    assert!(
        restored.status(10, None).await.unwrap().entries.iter().all(|entry| entry
            .capture
            .revision
            .as_ref()
            .unwrap()
            .sequence
            .parse::<u64>()
            .unwrap()
            > high)
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PhaseCall {
    Heads(Option<String>),
    Inputs(Option<String>),
}
struct ScriptedStore {
    id: String,
    heads: Mutex<VecDeque<DiagnosticHeadPage>>,
    inputs: Mutex<VecDeque<RecoveryPage>>,
    calls: Mutex<Vec<PhaseCall>>,
    cancel_after_head: Option<CollectionCancellation>,
}
impl ScriptedStore {
    fn new(heads: Vec<DiagnosticHeadPage>, inputs: Vec<RecoveryPage>) -> Self {
        Self {
            id: digest(b"scripted-recovery"),
            heads: Mutex::new(heads.into()),
            inputs: Mutex::new(inputs.into()),
            calls: Mutex::new(vec![]),
            cancel_after_head: None,
        }
    }
}
#[async_trait::async_trait]
impl RecoveryStore for ScriptedStore {
    fn id(&self) -> &str {
        &self.id
    }
    async fn diagnostic_heads(&self, cursor: Option<&str>, limit: u32) -> Result<DiagnosticHeadPage> {
        assert_eq!(limit, RESTORE_PAGE_ROWS);
        self.calls.lock().unwrap().push(PhaseCall::Heads(cursor.map(str::to_owned)));
        let page = self.heads.lock().unwrap().pop_front().expect("bounded checkpoint page");
        if let Some(cancel) = &self.cancel_after_head {
            cancel.cancel();
        }
        Ok(page)
    }
    async fn pending_diagnostic_inputs(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        assert_eq!(limit, RESTORE_PAGE_ROWS);
        self.calls.lock().unwrap().push(PhaseCall::Inputs(cursor.map(str::to_owned)));
        Ok(self.inputs.lock().unwrap().pop_front().expect("bounded original input page"))
    }
    async fn diagnostic_input_pending(&self, _: &DiagnosticCaptureRef, _: u32) -> Result<bool> {
        // These scripted pages contain no native inputs. An unimplemented exact lookup is unknown, not pending.
        Err(DeliveryProblem::UnsupportedSchema.error())
    }
    async fn pending_ranges(&self, _: Option<&str>, _: u32, _: RecoveryOrder) -> Result<RecoveryPage> {
        panic!("publication lag is not pending diagnostic work")
    }
    async fn recover(&self, _: Option<&str>, _: u32) -> Result<RecoveryPage> {
        panic!("startup does not replay all history")
    }
    async fn checkpoint_head(&self) -> Result<Option<String>> {
        panic!("no remote write during restoration")
    }
    async fn captured_range(&self, _: &CapturedRange) -> Result<Option<CaptureIdentity>> {
        panic!("no native lookup")
    }
    async fn stage(&self, _: &PayloadObject, _: &Path) -> Result<StagedBatch> {
        panic!("no staging")
    }
    async fn commit(&self, _: &Fence, _: Option<&str>, _: &RangeManifest, _: &[StagedBatch]) -> Result<CommitReceipt> {
        panic!("no commit")
    }
    async fn load(&self, _: &str) -> Result<RangeManifest> {
        panic!("empty pages have no object")
    }
    async fn capture_manifest(&self, _: &CaptureIdentity) -> Result<RangeManifest> {
        panic!("empty restore pages have no focused parent")
    }
    async fn retire(&self, _: &Fence, _: &culprit_collector::delivery::sink::PublishedRange) -> Result<()> {
        panic!("no retirement")
    }
    async fn open_object(&self, _: &PayloadObject) -> Result<RecoveryReader> {
        panic!("empty pages have no payload")
    }
}
fn heads(sequence: u64, next: Option<&str>) -> DiagnosticHeadPage {
    DiagnosticHeadPage { heads: vec![], high_sequence: sequence, next_cursor: next.map(str::to_owned) }
}
fn inputs(sequence: u64, next: Option<&str>) -> RecoveryPage {
    RecoveryPage { ranges: vec![], high_sequence: sequence, next_cursor: next.map(str::to_owned) }
}
async fn empty_outbox(path: &Path, store: &ScriptedStore) -> Outbox {
    let mut outbox = Outbox::create_remote(path, OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox
}
#[tokio::test]
async fn restore_pages_keep_independent_cursors_order_and_maximum_high_sequence() {
    let root = tempfile::tempdir().unwrap();
    let store = ScriptedStore::new(
        vec![heads(9, Some("next-head")), heads(3, None)],
        vec![inputs(15, Some("next-input")), inputs(10, None)],
    );
    let mut outbox = empty_outbox(&root.path().join("outbox"), &store).await;
    assert_eq!(restore_diagnostic_state(&mut outbox, &store, &CollectionCancellation::default()).await.unwrap(), 15);
    assert_eq!(
        *store.calls.lock().unwrap(),
        vec![
            PhaseCall::Heads(None),
            PhaseCall::Heads(Some("next-head".into())),
            PhaseCall::Inputs(None),
            PhaseCall::Inputs(Some("next-input".into()))
        ]
    );
}
#[tokio::test]
async fn cancellation_after_a_checkpoint_page_stops_before_pending_inputs_or_epoch_activation() {
    let root = tempfile::tempdir().unwrap();
    let cancellation = CollectionCancellation::default();
    let mut store = ScriptedStore::new(vec![heads(9, None)], vec![]);
    store.cancel_after_head = Some(cancellation.clone());
    let mut outbox = empty_outbox(&root.path().join("outbox"), &store).await;
    assert_eq!(restore_diagnostic_state(&mut outbox, &store, &cancellation).await.unwrap(), 9);
    assert_eq!(*store.calls.lock().unwrap(), vec![PhaseCall::Heads(None)]);
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: Default::default(),
    };
    assert!(outbox.reserve("not-activated", &request).await.is_err());
}
#[tokio::test]
async fn cyclic_oversized_or_nonterminating_recovery_cursors_are_bounded() {
    for pages in [
        vec![heads(1, Some("repeat")), heads(2, Some("repeat"))],
        vec![DiagnosticHeadPage {
            heads: vec![],
            high_sequence: 1,
            next_cursor: Some("x".repeat(MAX_CURSOR_BYTES + 1)),
        }],
        (0..MAX_RESTORE_PAGES).map(|page| heads(page as u64, Some(&format!("page-{page}")))).collect(),
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = ScriptedStore::new(pages, vec![]);
        let mut outbox = empty_outbox(&root.path().join("outbox"), &store).await;
        let error =
            restore_diagnostic_state(&mut outbox, &store, &CollectionCancellation::default()).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        assert!(store.calls.lock().unwrap().len() <= MAX_RESTORE_PAGES);
    }
}

#[tokio::test]
async fn restoration_rejects_a_different_recovery_namespace_before_reading_work() {
    let root = tempfile::tempdir().unwrap();
    let mut store = ScriptedStore::new(vec![heads(1, None)], vec![]);
    let mut outbox = empty_outbox(&root.path().join("outbox"), &store).await;
    store.id = digest(b"foreign-recovery-namespace");
    let error = restore_diagnostic_state(&mut outbox, &store, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::DestinationChanged)));
    assert!(store.calls.lock().unwrap().is_empty());
}
