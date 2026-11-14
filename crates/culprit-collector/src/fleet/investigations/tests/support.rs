//! Sealed synthetic evidence for planner and runtime admission tests; it is never a production cost qualification.
use super::super::{
    RouteBindings,
    planning::{PreparedInvestigation, prepare},
};
use crate::{
    analytical::test_support::investigation_fixture,
    capture::{CollectionAdmissionWindow, CollectionCancellation, SourceFactory, collect},
    delivery::recovery::Fence,
    diagnostics::{DiagnosticPolicies, drain_pending},
    fleet::{FleetBackend, FleetCaptureOutcome, FleetJournal, FleetSourceAdmission},
    outbox::Outbox,
};
use culprit_core::{ComponentVersion, Result, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader, admission::SourceGate};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone)]
pub(in crate::fleet::investigations) struct Frame {
    pub(in crate::fleet::investigations) source: CollectionDescription,
    pub(in crate::fleet::investigations) records: Vec<TelemetryRecord>,
}
#[derive(Default)]
pub(in crate::fleet::investigations) struct NativeCalls {
    pub opened: AtomicUsize,
    pub observed: AtomicUsize,
    pub verified: AtomicUsize,
    pub closed: AtomicUsize,
    pub captured: AtomicUsize,
}
struct Factory {
    frame: Frame,
    admission: Option<CollectionAdmissionWindow>,
    intent: Option<DiagnosticInvestigationIntent>,
    calls: Arc<NativeCalls>,
    failure: Option<CollectionStop>,
    auth_failure: bool,
}
impl Factory {
    fn manual(frame: Frame) -> Self {
        Self { frame, admission: None, intent: None, calls: Arc::default(), failure: None, auth_failure: false }
    }
}
struct Reader {
    frame: Frame,
    queries: u32,
    calls: Arc<NativeCalls>,
    failure: Option<CollectionStop>,
}
#[async_trait::async_trait]
impl SourceFactory for Factory {
    fn admission_window(&self) -> Option<CollectionAdmissionWindow> {
        self.admission
    }
    fn investigation_intent(&self) -> Option<&DiagnosticInvestigationIntent> {
        self.intent.as_ref()
    }
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.calls.opened.fetch_add(1, Ordering::SeqCst);
        if self.auth_failure {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::AuthFailed,
                "fixture authentication rejected",
            ));
        }
        Ok(Box::new(FrozenSource::new(
            Reader { frame: self.frame.clone(), queries: 0, calls: self.calls.clone(), failure: self.failure },
            request,
        )?))
    }
}
#[async_trait::async_trait]
impl ObservationReader for Reader {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<FrozenObservation> {
        self.queries += 1;
        self.calls.observed.fetch_add(1, Ordering::SeqCst);
        if let Some(failure) = self.failure {
            return Err(failure.error());
        }
        Ok(FrozenObservation { description: self.frame.source.clone(), records: self.frame.records.clone() })
    }
    async fn verify(&mut self, source: &CollectionDescription) -> Result<bool> {
        self.queries += 1;
        self.calls.verified.fetch_add(1, Ordering::SeqCst);
        Ok(*source == self.frame.source)
    }
    fn query_count(&self) -> u32 {
        self.queries
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.calls.closed.fetch_add(1, Ordering::SeqCst);
        if self.failure == Some(CollectionStop::CancellationUnconfirmed) {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        }
    }
}

pub(in crate::fleet::investigations) struct Backend {
    pub(in crate::fleet::investigations) recipe: RecipeDescriptor,
    pub(in crate::fleet::investigations) component: ComponentVersion,
    pub(in crate::fleet::investigations) revision: String,
    pub(in crate::fleet::investigations) verified: AtomicUsize,
    pub(in crate::fleet::investigations) components: AtomicUsize,
    pub(in crate::fleet::investigations) accept: bool,
    pub(in crate::fleet::investigations) fail: bool,
    pub(in crate::fleet::investigations) cancel: Option<CollectionCancellation>,
    pub(in crate::fleet::investigations) changed: AtomicBool,
    pub(in crate::fleet::investigations) route_error: Option<culprit_core::Error>,
    pub(in crate::fleet::investigations) activity: Frame,
    pub(in crate::fleet::investigations) focus_request: PageWaitRequestState,
    pub(in crate::fleet::investigations) focus_failure: Option<CollectionStop>,
    pub(in crate::fleet::investigations) focus_auth_failure: bool,
    pub(in crate::fleet::investigations) calls: Arc<NativeCalls>,
}
#[async_trait::async_trait]
impl FleetBackend for Backend {
    fn validate_target(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn telemetry_recipes(&self, _: &str) -> Result<Vec<RecipeDescriptor>> {
        Ok(vec![self.recipe.clone()])
    }
    fn route_revision(&self, _: &str) -> Result<Option<String>> {
        if let Some(error) = &self.route_error {
            return Err(error.clone());
        }
        Ok(Some(if self.changed.load(Ordering::SeqCst) { digest(b"changed-route") } else { self.revision.clone() }))
    }
    fn qualification_component(&self, _: &str) -> Result<Option<ComponentVersion>> {
        self.components.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.component.clone()))
    }
    fn verify_cost(
        &self,
        _: &str,
        receipt: &RecipeCostReceipt,
        source: &CollectionDescription,
        row: &ExportedRecord,
    ) -> Result<bool> {
        self.verified.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            DiagnosticEvidenceRef::from_export(
                source,
                &receipt.evidence.manifest_sha256,
                row,
                receipt.evidence.row_ordinal
            )?,
            receipt.evidence
        );
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        if self.fail {
            return Err(DeliveryProblem::Integrity.error());
        }
        Ok(self.accept)
    }
    async fn discover(
        &self,
        _: &str,
        _: &IntervalDiscoveryRequest,
        _: FleetSourceAdmission,
    ) -> Result<IntervalDiscoveryPage> {
        panic!("planning must never discover a native source")
    }
    async fn capture(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        admission: FleetSourceAdmission,
    ) -> FleetCaptureOutcome {
        let gate = SourceGate::default();
        let mut source_work = SourceWorkState::NotStarted;
        let mut attempt = None;
        let result = async {
            assert_eq!(connection, "saved-source");
            admission.window.check_start()?;
            assert_eq!(admission.route_revision.as_deref(), Some(self.revision.as_str()));
            let frame = match &request.selection {
                CollectionSelection::Activity => {
                    assert!(admission.investigation.is_none());
                    self.activity.clone()
                }
                CollectionSelection::FocusedPageWait { focus } => {
                    let intent =
                        admission.investigation.as_ref().expect("automatic focus must retain its admitted intent");
                    assert_eq!(&intent.focus, focus.as_ref());
                    assert_eq!(intent.budget, request.budget);
                    focused_frame(focus.as_ref().clone(), self.focus_request)
                }
                _ => panic!("runtime fixture only implements its declared activity and focused routes"),
            };
            self.calls.captured.fetch_add(1, Ordering::SeqCst);
            let factory = Factory {
                frame,
                admission: Some(admission.window),
                intent: admission.investigation,
                calls: self.calls.clone(),
                failure: matches!(request.selection, CollectionSelection::FocusedPageWait { .. })
                    .then_some(self.focus_failure)
                    .flatten(),
                auth_failure: matches!(request.selection, CollectionSelection::FocusedPageWait { .. })
                    && self.focus_auth_failure,
            };
            let mut lease = outbox.reserve(connection, &request).await?;
            let capture = lease.identity().clone();
            attempt = Some(DiagnosticAttemptReceipt { capture: capture.clone(), manifest_sha256: None });
            let owner = factory.intent.as_ref().map(DiagnosticInvestigationLineage::from_intent).transpose()?;
            let directory = lease.directory().to_owned();
            let captured =
                collect(&gate, &factory, request, &directory, capture.clone(), cancellation, Some(&mut lease)).await;
            source_work = if gate.is_closed() { SourceWorkState::Unconfirmed } else { SourceWorkState::Quiescent };
            let finished = lease.finish().await;
            attempt = Some(outbox.attempt_receipt(&capture, owner.as_ref()).await?);
            finished?;
            captured
        }
        .await;
        FleetCaptureOutcome { result, source_work, attempt }
    }
}

pub(in crate::fleet::investigations) struct Harness {
    pub(in crate::fleet::investigations) _directory: tempfile::TempDir,
    pub(in crate::fleet::investigations) inventory: FleetInventory,
    pub(in crate::fleet::investigations) outbox: Outbox,
    pub(in crate::fleet::investigations) journal: FleetJournal,
    pub(in crate::fleet::investigations) bindings: RouteBindings,
    pub(in crate::fleet::investigations) backend: Backend,
}
impl Harness {
    pub(in crate::fleet::investigations) async fn new(rows: usize) -> Self {
        Self::with_recovery_store(rows, None).await
    }

    pub(in crate::fleet::investigations) async fn with_recovery_store(rows: usize, fence: Option<&Fence>) -> Self {
        let fixture = investigation_fixture();
        let directory = tempfile::tempdir().unwrap();
        let inventory = FleetInventory {
            targets: vec![FleetTarget {
                id: "target".into(),
                connection: "saved-source".into(),
                subject: CollectionSubject::Database,
                shard: "one".into(),
                admission_group: "physical-resource".into(),
                schedules: vec![FleetSchedule {
                    id: "activity".into(),
                    every_seconds: 86_400,
                    jitter_seconds: 0,
                    collection: ScheduledCollection::Activity { policy: ActivityPolicy::default() },
                    budget: CollectionBudget::default(),
                }],
            }],
        };
        let path = directory.path().join("outbox");
        let mut outbox = match fence {
            Some(fence) => Outbox::create_remote(&path, OutboxLimits::default(), fence.store_id()),
            None => Outbox::create(&path, OutboxLimits::default()),
        }
        .unwrap();
        outbox.recover().await.unwrap();
        if let Some(fence) = fence {
            outbox.begin_remote_epoch(fence, 0).await.unwrap();
        }
        let mut source = fixture.baseline_source.clone();
        let mut records = Vec::new();
        for index in 0..rows {
            let mut row = fixture.baseline_record.record.clone();
            let TelemetryRecord::Activity(original) = &mut row else { panic!() };
            original.key.session.id = (100 + index).to_string();
            records.push(row);
        }
        let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
        frame.native_rows = rows as u32;
        frame.observations = rows as u32;
        frame.coverage.records = rows as u32;
        let activity = Frame { source: source.clone(), records };
        seal(&mut outbox, activity.clone()).await;
        let cost =
            seal(&mut outbox, Frame { source: fixture.source.clone(), records: vec![fixture.record.record.clone()] })
                .await;
        let policies = DiagnosticPolicies::from_inventory(&inventory, "one").unwrap();
        drain_pending(&mut outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
        let mut recipe = fixture.recipe;
        let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
        receipt.evidence = cost;
        let revision = digest(b"saved-route-revision");
        let mut bindings = RouteBindings::default();
        bindings.observe("target", &source, &revision).unwrap();
        let journal = FleetJournal::open(&directory.path().join("fleet"), &inventory, "one").await.unwrap();
        Self {
            _directory: directory,
            inventory,
            outbox,
            journal,
            bindings,
            backend: Backend {
                recipe,
                component: fixture.intent.qualification.component,
                revision,
                verified: AtomicUsize::new(0),
                components: AtomicUsize::new(0),
                accept: true,
                fail: false,
                cancel: None,
                changed: AtomicBool::new(false),
                route_error: None,
                activity,
                focus_request: PageWaitRequestState::Missing,
                focus_failure: None,
                focus_auth_failure: false,
                calls: Arc::default(),
            },
        }
    }
    pub(in crate::fleet::investigations) async fn prepare(
        &mut self,
        cancellation: &CollectionCancellation,
    ) -> Result<Vec<PreparedInvestigation>> {
        prepare(
            &self.backend,
            &self.journal,
            &mut self.outbox,
            None,
            &self.inventory.targets,
            &self.bindings,
            cancellation,
        )
        .await
    }
}

fn focused_frame(focus: ActivityWaitFocus, request: PageWaitRequestState) -> Frame {
    let at = chrono::DateTime::parse_from_rfc3339(&focus.original_observed_at).unwrap();
    let from = (at + chrono::Duration::seconds(1)).to_rfc3339();
    let until = (at + chrono::Duration::seconds(2)).to_rfc3339();
    let source = "fixture-focused-pages".to_owned();
    let result = ActivityWaitFocusResult {
        focus,
        observed_from: from.clone(),
        observed_until: until.clone(),
        source: FocusSourceState::Matched,
        request,
        wait: PageWaitRelation::Unqualified,
    };
    assert!(result.is_valid());
    Frame {
        source: CollectionDescription {
            target: result.focus.target.clone(),
            source: source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedPageWait {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: from,
                    observed_until: until,
                    records: 0,
                    sources: vec![StateSourceCoverage::observed(source, 0, false)],
                },
                result: Box::new(result),
            },
        },
        records: Vec::new(),
    }
}

pub(in crate::fleet::investigations) async fn seal(outbox: &mut Outbox, frame: Frame) -> DiagnosticEvidenceRef {
    let source = frame.source.clone();
    let first = frame.records[0].clone();
    let (manifest, directory) = capture_artifact(outbox, frame).await;
    let first = ExportedRecord::new(&source, &manifest.capture, first).unwrap();
    DiagnosticEvidenceRef::from_export(
        &source,
        &digest(&std::fs::read(directory.join("manifest.json")).unwrap()),
        &first,
        0,
    )
    .unwrap()
}

pub(in crate::fleet::investigations) async fn capture_frame(outbox: &mut Outbox, frame: Frame) -> CaptureIdentity {
    capture_artifact(outbox, frame).await.0.capture
}

async fn capture_artifact(outbox: &mut Outbox, frame: Frame) -> (CollectionManifest, std::path::PathBuf) {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: match &frame.source.observation {
            SourceObservation::Activity(_) => CollectionSelection::Activity,
            SourceObservation::Snapshot(snapshot) => CollectionSelection::Snapshot { dataset: snapshot.dataset },
            _ => panic!(),
        },
        budget: CollectionBudget { page_rows: 32, ..Default::default() },
    };
    let mut lease = outbox.reserve("saved-source", &request).await.unwrap();
    let capture = lease.identity().clone();
    let directory = lease.directory().to_owned();
    let manifest = collect(
        &SourceGate::default(),
        &Factory::manual(frame),
        request,
        &directory,
        capture,
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await
    .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    lease.finish().await.unwrap();
    (manifest, directory)
}
