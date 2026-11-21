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
use culprit_core::{ComponentVersion, Engine, MeasurementScope, Result, telemetry::*};
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
    pub(in crate::fleet::investigations) additional_recipes: Vec<RecipeDescriptor>,
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
    pub(in crate::fleet::investigations) shared: Option<Frame>,
    pub(in crate::fleet::investigations) current_work: Option<Frame>,
    pub(in crate::fleet::investigations) source_read_result: Option<Frame>,
    pub(in crate::fleet::investigations) compiler_gateways: Option<Vec<OperationalObservation>>,
    pub(in crate::fleet::investigations) page_tasks: Option<Frame>,
    pub(in crate::fleet::investigations) focus_request: PageWaitRequestState,
    pub(in crate::fleet::investigations) focus_resolution: Option<PageWaitResolution>,
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
        Ok(std::iter::once(self.recipe.clone()).chain(self.additional_recipes.iter().cloned()).collect())
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
        assert_eq!(source.target, receipt.scope.target(), "cost proof belongs to the selected execution resource");
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
                CollectionSelection::Snapshot { dataset: SnapshotDataset::Cleanup }
                    if self.activity.source.observation.collection() == request.collection() =>
                {
                    assert!(admission.investigation.is_none());
                    self.activity.clone()
                }
                CollectionSelection::FocusedPageWait { focus } => {
                    let intent =
                        admission.investigation.as_ref().expect("automatic focus must retain its admitted intent");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    assert_eq!(intent.budget, request.budget);
                    if self.focus_resolution == Some(PageWaitResolution::ObservedAtBothBounds) {
                        super::page_writers::mapped_frame(focus)
                    } else {
                        focused_frame(focus.as_ref().clone(), self.focus_request, self.focus_resolution)
                    }
                }
                CollectionSelection::PageWaitTasks { activity, original_context, .. } => {
                    let intent =
                        admission.investigation.as_ref().expect("paired population retains its exact original trigger");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    assert_eq!(intent.budget, request.budget);
                    self.page_tasks.clone().unwrap_or_else(|| {
                        super::page_tasks::population_frame(activity, original_context, &self.activity)
                    })
                }
                CollectionSelection::PageTaskHolders { focus } => {
                    let intent = admission
                        .investigation
                        .as_ref()
                        .expect("task-holder work retains both exact original receipts");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    assert_eq!(intent.budget, request.budget);
                    super::page_task_holders::focused_frame(focus)
                }
                CollectionSelection::PageWaitWriters { focus } => {
                    let intent =
                        admission.investigation.as_ref().expect("writer work retains its exact admitted roster");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    assert_eq!(intent.budget, request.budget);
                    super::page_writers::focused_frame(focus, &self.activity)
                }
                CollectionSelection::SourceRead { focus, execution } => {
                    let intent =
                        admission.investigation.as_ref().expect("source history retains its exact admitted question");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    self.source_read_result.clone().unwrap_or_else(|| match focus.question {
                        SourceReadQuestion::CompilationHistory => {
                            if matches!(focus.prerequisite, SourceReadPrerequisite::ObservedQueryHistoryFailure { .. })
                            {
                                super::source_reads::query_history::frame(&self.activity, focus, execution)
                            } else {
                                super::source_reads::frame(
                                    focus,
                                    execution.logical().expect("logical compilation owner"),
                                )
                            }
                        }
                        SourceReadQuestion::HyperscaleLogComponents => super::source_reads::log_rates::frame(
                            focus,
                            execution.logical().expect("logical log owner"),
                        ),
                        SourceReadQuestion::TemporarySpaceContributors => {
                            super::source_reads::temporary::frame(focus, execution)
                        }
                    })
                }
                CollectionSelection::FocusedCompilationAdmission { focus } => {
                    let intent =
                        admission.investigation.as_ref().expect("automatic compiler question retains its intent");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    assert_eq!(intent.budget, request.budget);
                    super::compiler_admission::focused_frame(focus, self.compiler_gateways.as_deref())
                }
                CollectionSelection::FocusedRequestMemory { focus } => {
                    let intent = admission
                        .investigation
                        .as_ref()
                        .expect("automatic memory question retains its admitted intent");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    assert_eq!(intent.budget, request.budget);
                    super::request_memory::focused_frame(focus)
                }
                CollectionSelection::CurrentRequestWork { focus } => {
                    let intent =
                        admission.investigation.as_ref().expect("current work retains its admitted native request");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    assert_eq!(intent.budget, request.budget);
                    self.current_work.clone().unwrap_or_else(|| super::current_request_work::focused_frame(focus))
                }
                CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational } => {
                    assert!(admission.investigation.is_none());
                    self.shared.clone().expect("the shared route must have an independently observed frame")
                }
                CollectionSelection::SharedPageWaitFocus { focus, partition } => {
                    let intent = admission.investigation.as_ref().expect("shared work retains its admitted intent");
                    assert_eq!(intent.selection().unwrap(), request.selection);
                    assert_eq!(intent.subject(), request.subject);
                    assert_eq!(intent.budget, request.budget);
                    let mut frame = focused_frame(focus.as_ref().clone(), self.focus_request, self.focus_resolution);
                    let CollectionSubject::Shared { resource } = &request.subject else { panic!() };
                    frame.source.target = CollectionTarget::Shared(resource.clone());
                    frame.source.partition = partition.clone();
                    frame
                }
                _ => panic!("runtime fixture only implements its declared activity and focused routes"),
            };
            self.calls.captured.fetch_add(1, Ordering::SeqCst);
            let investigation = admission.investigation.is_some();
            let factory = Factory {
                frame,
                admission: Some(admission.window),
                intent: admission.investigation,
                calls: self.calls.clone(),
                failure: investigation.then_some(self.focus_failure).flatten(),
                auth_failure: investigation && self.focus_auth_failure,
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
enum FixtureRoutes {
    Database,
    Shared,
    Progressive,
}
impl Harness {
    pub(in crate::fleet::investigations) async fn new(rows: usize) -> Self {
        Self::with_recovery_store(rows, None).await
    }

    pub(in crate::fleet::investigations) async fn with_recovery_store(rows: usize, fence: Option<&Fence>) -> Self {
        Self::build(rows, fence, FixtureRoutes::Database, None).await
    }

    pub(in crate::fleet::investigations) async fn with_query_history(source: Frame) -> Self {
        assert!(matches!(source.source.observation, SourceObservation::QueryHistory(_)));
        Self::build(0, None, FixtureRoutes::Database, Some(source)).await
    }

    pub(in crate::fleet::investigations) async fn with_archived_query_history(source: Frame, fence: &Fence) -> Self {
        assert!(matches!(source.source.observation, SourceObservation::QueryHistory(_)));
        Self::build(0, Some(fence), FixtureRoutes::Database, Some(source)).await
    }

    pub(in crate::fleet::investigations) async fn with_activity(activity: Frame) -> Self {
        Self::build(0, None, FixtureRoutes::Database, Some(activity)).await
    }

    pub(in crate::fleet::investigations) async fn with_archived_activity(activity: Frame, fence: &Fence) -> Self {
        Self::build(0, Some(fence), FixtureRoutes::Database, Some(activity)).await
    }

    pub(in crate::fleet::investigations) async fn shared() -> Self {
        Self::build(1, None, FixtureRoutes::Shared, None).await
    }

    pub(in crate::fleet::investigations) async fn progressive() -> Self {
        Self::build(1, None, FixtureRoutes::Progressive, None).await
    }

    async fn build(rows: usize, fence: Option<&Fence>, routes: FixtureRoutes, supplied: Option<Frame>) -> Self {
        let fixture = investigation_fixture();
        let directory = tempfile::tempdir().unwrap();
        let mut inventory = FleetInventory {
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
        let shared = (!matches!(routes, FixtureRoutes::Database)).then(shared_frame);
        if let Some(shared) = &shared {
            let CollectionTarget::Shared(member) = &shared.source.target else { panic!() };
            source.partition = SourcePartition::InstanceLocal {
                member: Some(member.clone()),
                epoch: Some("native-database-generation".into()),
            };
            let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
            frame.epoch = Some("native-database-generation".into());
            let target = &mut inventory.targets[0];
            target.subject = CollectionSubject::Shared { resource: member.clone() };
            target.schedules[0].collection = ScheduledCollection::Snapshot { dataset: SnapshotDataset::Operational };
            if matches!(routes, FixtureRoutes::Progressive) {
                let mut database = target.clone();
                database.id = "database-target".into();
                database.subject = CollectionSubject::Database;
                database.schedules[0].collection = ScheduledCollection::Activity { policy: ActivityPolicy::default() };
                inventory.targets.insert(0, database);
            }
        }
        let mut records = Vec::new();
        for index in 0..rows {
            let mut row = fixture.baseline_record.record.clone();
            let TelemetryRecord::Activity(original) = &mut row else { panic!() };
            original.key.session.id = (100 + index).to_string();
            if shared.is_some() {
                original.wait.as_mut().unwrap().identified_resources[0].native.id = "2:1:4144".into();
            }
            records.push(row);
        }
        let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
        frame.native_rows = rows as u32;
        frame.observations = rows as u32;
        frame.coverage.records = rows as u32;
        let activity = supplied.unwrap_or_else(|| Frame { source: source.clone(), records });
        source = activity.source.clone();
        if let SourceObservation::LocalResource { snapshot, .. } = &source.observation {
            inventory.targets[0].schedules[0].collection = ScheduledCollection::Snapshot { dataset: snapshot.dataset };
        } else if matches!(source.observation, SourceObservation::QueryHistory(_)) {
            let schedule = &mut inventory.targets[0].schedules[0];
            schedule.collection = ScheduledCollection::QueryHistory {
                lookback_seconds: schedule.every_seconds.max(3600),
                reconcile_seconds: schedule.every_seconds,
            };
        }
        seal(&mut outbox, activity.clone()).await;
        let mut cost_frame = Frame { source: fixture.source.clone(), records: vec![fixture.record.record.clone()] };
        cost_frame.source.target = source.target.clone();
        if let Some(shared) = &shared {
            cost_frame.source.target = shared.source.target.clone();
            let TelemetryRecord::Operational(row) = &mut cost_frame.records[0] else { panic!() };
            let OperationalFact::CounterSet(counters) = &mut row.fact else { panic!() };
            counters.scope = MeasurementScope::Server;
            for sample in &mut counters.counters {
                sample.scope = MeasurementScope::Server;
            }
            row.key = counters.key().unwrap();
        }
        let cost = seal(&mut outbox, cost_frame).await;
        let policies = DiagnosticPolicies::from_inventory(&inventory, "one").unwrap();
        drain_pending(&mut outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
        let mut recipe = fixture.recipe;
        let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
        receipt.evidence = cost;
        receipt.scope = RecipeCostScope::Activity(DiagnosticScope::from_source(&source).unwrap());
        if let Some(shared) = &shared {
            let CollectionTarget::Shared(resource) = &shared.source.target else { panic!() };
            receipt.scope = RecipeCostScope::SharedResource {
                target: resource.clone(),
                partition: shared.source.partition.clone(),
                source: shared.source.source.clone(),
            };
            recipe.subjects = vec![RecipeSubject::Shared(resource.kind)];
        }
        let mut additional_recipes = Vec::new();
        if matches!(routes, FixtureRoutes::Progressive) {
            let database_cost = seal(
                &mut outbox,
                Frame { source: fixture.source.clone(), records: vec![fixture.record.record.clone()] },
            )
            .await;
            let mut database_recipe = recipe.clone();
            database_recipe.id = "fixture.database-page-wait-focus".into();
            database_recipe.subjects = vec![RecipeSubject::Database];
            let RecipeCostQualification::Measured(receipt) = &mut database_recipe.cost else { panic!() };
            receipt.recipe_id = database_recipe.id.clone();
            receipt.scope = RecipeCostScope::Activity(DiagnosticScope::from_source(&source).unwrap());
            receipt.evidence = database_cost;
            additional_recipes.push(database_recipe);
        }
        let revision = digest(b"saved-route-revision");
        let mut bindings = RouteBindings::default();
        if shared.is_none() {
            bindings.observe("target", &source, &revision).unwrap();
        }
        let journal = FleetJournal::open(&directory.path().join("fleet"), &inventory, "one").await.unwrap();
        Self {
            _directory: directory,
            inventory,
            outbox,
            journal,
            bindings,
            backend: Backend {
                recipe,
                additional_recipes,
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
                shared,
                current_work: None,
                source_read_result: None,
                compiler_gateways: None,
                page_tasks: None,
                focus_request: PageWaitRequestState::Missing,
                focus_resolution: None,
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

fn shared_frame() -> Frame {
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("fixture-native-instance-namespace"),
        native: NativeIdentity::new("fixture-native-instance"),
    };
    let source = "fixture-shared-native-owner".to_owned();
    let counters = NativeCounterSet {
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        identity: NativeIdentity::new("shared-native-counter-point"),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "latch_waiters",
            1,
            CounterUnit::Count,
            MeasurementScope::Server,
            "fixture-native-counters",
            "latch_waiters",
        )],
        properties: Default::default(),
    };
    Frame {
        source: CollectionDescription {
            target: CollectionTarget::Shared(member.clone()),
            partition: SourcePartition::InstanceLocal {
                member: Some(member),
                epoch: Some("native-instance-generation".into()),
            },
            source: source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::Operational,
                observed_from: "2020-01-01T12:00:00.500Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(source.clone(), 1)],
            }),
        },
        records: vec![TelemetryRecord::Operational(OperationalObservation {
            source,
            key: counters.key().unwrap(),
            fact: OperationalFact::CounterSet(Box::new(counters)),
        })],
    }
}

fn focused_frame(
    focus: ActivityWaitFocus,
    request: PageWaitRequestState,
    resolution: Option<PageWaitResolution>,
) -> Frame {
    let at = chrono::DateTime::parse_from_rfc3339(&focus.participant().original_observed_at).unwrap();
    let from = (at + chrono::Duration::seconds(1)).to_rfc3339();
    let until = (at + chrono::Duration::seconds(2)).to_rfc3339();
    let source = "fixture-focused-pages".to_owned();
    let records = resolution
        .map(|resolution| {
            let page = PageWaitObservation {
                source: source.clone(),
                observed_from: from.clone(),
                observed_until: until.clone(),
                request: focus.participant().original.clone(),
                task_wait: None,
                before_check: Some(PageWaitRecheck {
                    observed_at: from.clone(),
                    request,
                    wait: PageWaitRelation::OriginalObserved,
                }),
                after_check: Some(PageWaitRecheck {
                    observed_at: until.clone(),
                    request,
                    wait: PageWaitRelation::OriginalObserved,
                }),
                statement_identity: None,
                plan_identity: None,
                resource: focus.resource.clone(),
                observed_address: Some(PageWaitAddress { database_id: 2, file_id: 1, page_id: 4144 }),
                resolution,
                target: None,
                index_operations: None,
            };
            assert!(page.is_valid());
            TelemetryRecord::Operational(OperationalObservation {
                source: source.clone(),
                key: page.key().unwrap(),
                fact: OperationalFact::PageWait(Box::new(page)),
            })
        })
        .into_iter()
        .collect::<Vec<_>>();
    let result = ActivityWaitFocusResult {
        focus,
        observed_from: from.clone(),
        observed_until: until.clone(),
        source: FocusSourceState::Matched,
        request,
        wait: if records.is_empty() { PageWaitRelation::Unqualified } else { PageWaitRelation::OriginalObserved },
    };
    assert!(result.is_valid());
    Frame {
        source: CollectionDescription {
            partition: result.focus.participant().partition.clone(),
            target: result.focus.participant().target.clone(),
            source: source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedPageWait {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: from,
                    observed_until: until,
                    records: records.len() as u32,
                    sources: vec![StateSourceCoverage::observed(source, records.len() as u32, false)],
                },
                result: Box::new(result),
            },
        },
        records,
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
        subject: match &frame.source.target {
            CollectionTarget::Database(_) => CollectionSubject::Database,
            CollectionTarget::Shared(resource) => CollectionSubject::Shared { resource: resource.clone() },
        },
        selection: match &frame.source.observation {
            SourceObservation::Activity(_) => CollectionSelection::Activity,
            SourceObservation::QueryHistory(history) => CollectionSelection::QueryHistory {
                from: history.interval.from.clone(),
                until: history.interval.until.clone(),
            },
            SourceObservation::PageWaitTargets { original_activity: Some(activity), .. } => {
                let original = outbox
                    .diagnostic_input(&activity.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
                    .await
                    .unwrap();
                let frame = original.activity_frame().unwrap();
                CollectionSelection::PageWaitTasks {
                    original_requests: frame.page_wait_task_requests().unwrap(),
                    activity: activity.clone(),
                    original_context: Box::new(frame.context()),
                }
            }
            SourceObservation::PageWaitTargets { .. } => {
                CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets }
            }
            SourceObservation::Snapshot(snapshot) | SourceObservation::LocalResource { snapshot, .. } => {
                CollectionSelection::Snapshot { dataset: snapshot.dataset }
            }
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
