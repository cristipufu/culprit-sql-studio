use super::*;
use crate::{NewConnection, ProviderConstructionPolicy, ProviderRegistration};
use culprit_collector::{
    analytical::test_support::{QualificationFixture, QualificationFixtureCase},
    fleet::FleetJournal,
    outbox::{NativeQualificationSource, Outbox},
};
use culprit_core::{
    AuthKind, Engine, ExplainMode, LiveState, Plan, Provider, RecipeQualificationState, ResultSet, ServerInfo,
    Snapshot, SnapshotOptions,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct NativeState {
    context: Mutex<NativeReadContext>,
    factories: AtomicUsize,
    source_queries: AtomicUsize,
    uncertain_close: AtomicBool,
}
fn native_state(fixture: &QualificationFixture) -> Arc<NativeState> {
    let (observed_from, observed_until) = if let Some(header) = fixture.source.observation.snapshot() {
        (header.observed_from.clone(), header.observed_until.clone())
    } else {
        let header = fixture.source.observation.activity().unwrap();
        (header.observed_from.clone(), header.observed_until.clone())
    };
    Arc::new(NativeState {
        context: Mutex::new(NativeReadContext {
            target: fixture.scope.target.clone().into(),
            partition: fixture.scope.partition.clone(),
            database_writable: Some(fixture.scope.database_writable),
            observed_from,
            observed_until,
        }),
        factories: AtomicUsize::new(0),
        source_queries: AtomicUsize::new(0),
        uncertain_close: AtomicBool::new(false),
    })
}
struct NativeProvider {
    fixture: QualificationFixture,
}
#[async_trait::async_trait]
impl Provider for NativeProvider {
    fn engine(&self) -> Engine {
        Engine::Mssql
    }
    fn telemetry_source(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        assert!(self.fixture.source.matches_request(request));
        Ok(self.fixture.telemetry_source())
    }
    async fn probe(&self) -> Result<ServerInfo> {
        panic!("no native probe")
    }
    async fn snapshot(&self, _: &str, _: &SnapshotOptions) -> Result<Snapshot> {
        panic!("no scan")
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        panic!("no application execution")
    }
    async fn live(&self) -> Result<LiveState> {
        panic!("no parallel activity reader")
    }
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        panic!("no ad-hoc query")
    }
}
struct ProfileRead {
    source: Box<dyn TelemetrySource>,
    profile: SourceProfileAdmission,
    native: Arc<NativeState>,
}
impl ProfileRead {
    fn verify_owner(&self) -> Result<()> {
        if !self.profile.matches_native_context(&self.native.context.lock().unwrap()) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl TelemetrySource for ProfileRead {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.verify_owner()?;
        self.native.source_queries.fetch_add(1, Ordering::SeqCst);
        self.source.describe().await
    }
    fn query_count(&self) -> u32 {
        self.source.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        if self.source.query_count() == 0 {
            Some(ReadWorkMetrics {
                connection_work: None,
                native_session_work: None,
                active_tasks: 0,
                untracked_tasks: 0,
                templates: vec![],
            })
        } else {
            self.source.read_metrics()
        }
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.native.source_queries.fetch_add(1, Ordering::SeqCst);
        self.source.read_page(stream, cursor).await
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.verify_owner()?;
        self.source.finish().await
    }
    async fn close(&mut self) -> SourceShutdown {
        let original = self.source.close().await;
        if self.native.uncertain_close.load(Ordering::SeqCst) { SourceShutdown::Unconfirmed } else { original }
    }
}
fn registration(
    fixture: &QualificationFixture,
    native: Arc<NativeState>,
    profile_intake: bool,
) -> ProviderRegistration {
    let ordinary = fixture.clone();
    let calls = native.clone();
    let mut registration = ProviderRegistration::new(
        Arc::new(move |_, _context| {
            calls.factories.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(NativeProvider { fixture: ordinary.clone() }))
        }),
        ProviderConstructionPolicy::DeferredNativeWork,
        vec![fixture.recipe.clone()],
    )
    .unwrap();
    let policy = fixture.clone();
    registration = registration
        .with_qualification_authority(
            fixture.component.clone(),
            Arc::new(move |receipt, source, row| Ok(policy.verifies(receipt, source, row))),
        )
        .unwrap();
    let source = fixture.clone();
    let issuer = fixture.clone();
    registration = registration.with_native_qualification_issuer(
        Arc::new(move |_, request| {
            assert!(source.source.matches_request(request));
            Ok(source.telemetry_source())
        }),
        Arc::new(move |evidence| issuer.assess(evidence)),
    );
    if profile_intake {
        registration = registration
            .with_profile_source(
                vec![fixture.recipe.selector],
                Arc::new(move |provider, request, profile| {
                    Ok(Box::new(ProfileRead {
                        source: provider.telemetry_source(request)?,
                        profile: profile.clone(),
                        native: native.clone(),
                    }))
                }),
            )
            .unwrap();
    }
    registration
}
fn setup(
    root: &Path,
    fixture: &QualificationFixture,
    profile_intake: bool,
) -> (CollectorConfig, CollectorRuntime, Workspace, Arc<NativeState>) {
    let mut config = super::super::tests::config(root);
    config.inventory.targets[0].schedules.clear();
    config.source_credentials.clear();
    config.monitoring = MonitoringPolicy::Standard { budget: fixture.request().budget };
    let native = native_state(fixture);
    let runtime =
        CollectorRuntime::empty().with_provider(Engine::Mssql, registration(fixture, native.clone(), profile_intake));
    let workspace = runtime.workspace(&config.workspace);
    let mut stored = workspace.store.get("fixture").unwrap();
    stored.auth = AuthKind::None;
    workspace.store.put("fixture", stored).unwrap();
    config.validate().unwrap();
    (config, runtime, workspace, native)
}
async fn qualify(workspace: &Workspace, outbox: &mut Outbox, fixture: &QualificationFixture, name: &str) {
    let summary = workspace
        .qualify_recipe(name, &fixture.recipe.id, fixture.request(), outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(summary.state, RecipeQualificationState::Installed);
    assert!(fixture.closed());
}
async fn run_one(
    config: &CollectorConfig,
    workspace: &Workspace,
    plan: &Enrollment,
    outbox: &mut Outbox,
    journal: &Path,
    cancellation: &CollectionCancellation,
) -> (FleetSummary, Vec<FleetCollectionEvent>) {
    let inventory = plan.inventory.as_ref().unwrap();
    let journal = FleetJournal::open(journal, inventory, &config.shard).await.unwrap();
    let backend = plan.backend(workspace);
    let mut runner = FleetRunner::new(&backend, inventory, &config.shard, &journal, outbox).unwrap();
    let mut events = vec![];
    let result = runner
        .run(cancellation, Some(1), |event| {
            events.push(event.clone());
            Ok(())
        })
        .await
        .unwrap();
    (result, events)
}

#[tokio::test]
async fn standard_configuration_enrolls_a_trusted_baseline_and_uses_the_actual_fleet_factory_and_outbox_without_an_incident()
 {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (config, _, workspace, native) = setup(root.path(), &fixture, true);
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    let plan = Enrollment::resolve(&workspace, &config).unwrap();
    assert_eq!(plan.summary.assigned_targets, 1);
    assert_eq!(plan.summary.automatic.len(), 1);
    assert!(plan.summary.gaps.is_empty());
    assert_eq!(native.factories.load(Ordering::SeqCst), 0);
    let (summary, events) = run_one(
        &config,
        &workspace,
        &plan,
        &mut outbox,
        &root.path().join("fleet"),
        &CollectionCancellation::default(),
    )
    .await;
    assert_eq!((summary.complete, summary.incomplete), (1, 0));
    assert_eq!(native.factories.load(Ordering::SeqCst), 1);
    assert_eq!(native.source_queries.load(Ordering::SeqCst), 2);
    let capture = events[0].capture.as_ref().unwrap();
    let attempt = outbox.attempt_receipt(capture, None).await.unwrap();
    let original = outbox
        .delivery()
        .completed_snapshot_for_connection(
            "fixture",
            &attempt,
            SnapshotDataset::QueryCompilation,
            Default::default(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(original.records().len(), 1);
    assert_eq!(original.source(), &fixture.source);
    assert!(fixture.closed());
    assert!(outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    assert_eq!(workspace.collection_admission_status().await.unwrap().source_quiescence_required, false);
}

#[tokio::test]
async fn unqualified_and_missing_native_profile_intake_are_explicit_gaps_without_any_factory_work() {
    for intake in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
        let (config, _, workspace, native) = setup(root.path(), &fixture, intake);
        let plan = Enrollment::resolve(&workspace, &config).unwrap();
        assert!(plan.inventory.is_none());
        assert_eq!(plan.summary.gaps[0].reason, MonitoringGapReason::Unqualified);
        let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
        qualify(&workspace, &mut outbox, &fixture, "fixture").await;
        let plan = Enrollment::resolve(&workspace, &config).unwrap();
        if intake {
            assert!(plan.summary.gaps.is_empty());
        } else {
            assert!(plan.inventory.is_none());
            assert_eq!(plan.summary.gaps[0].reason, MonitoringGapReason::AdmissionUnsupported);
        }
        assert_eq!(native.factories.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn a_measured_catalog_copy_without_the_original_issued_binding_cannot_enroll_another_connection() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (mut config, runtime, workspace, native) = setup(root.path(), &fixture, true);
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    let mut copied = fixture.clone();
    copied.recipe = workspace.telemetry_recipes("fixture").unwrap().remove(0);
    let other_root = tempfile::tempdir().unwrap();
    config.workspace = other_root.path().join("workspace");
    let other =
        runtime.workspace(&config.workspace).with_provider(Engine::Mssql, registration(&copied, native.clone(), true));
    other
        .add_connection(NewConnection {
            name: "fixture".into(),
            input: "mssql://reader@fixture.invalid/sample".into(),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    let plan = Enrollment::resolve(&other, &config).unwrap();
    assert!(plan.inventory.is_none());
    assert_eq!(plan.summary.gaps[0].reason, MonitoringGapReason::MissingApproval);
    assert_eq!(native.factories.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn standard_enrollment_preserves_explicit_schedules_and_computes_a_feasible_shared_resource_period() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (mut config, _, workspace, _) = setup(root.path(), &fixture, true);
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    let explicit = FleetSchedule {
        id: "explicit-settings".into(),
        every_seconds: 3_600,
        jitter_seconds: 0,
        collection: ScheduledCollection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: CollectionBudget::default(),
    };
    config.inventory.targets[0].schedules.push(explicit.clone());
    let plan = Enrollment::resolve(&workspace, &config).unwrap();
    assert_eq!(plan.summary.explicit_schedules, 1);
    assert_eq!(plan.summary.automatic.len(), 1);
    let schedules = &plan.inventory.as_ref().unwrap().targets[0].schedules;
    assert_eq!(serde_json::to_value(&schedules[0]).unwrap(), serde_json::to_value(explicit).unwrap());
    assert!(plan.summary.automatic[0].every_seconds >= STANDARD_PERIOD_SECONDS);
    config.inventory.targets[0].schedules[0].every_seconds = MIN_ACTIVITY_POLL_SECONDS;
    let impossible = Enrollment::resolve(&workspace, &config).unwrap();
    assert!(impossible.summary.automatic.is_empty());
    assert_eq!(impossible.summary.gaps[0].reason, MonitoringGapReason::InfeasibleCadence);
    assert_eq!(impossible.inventory.unwrap().targets[0].schedules.len(), 1);
}

#[tokio::test]
async fn the_same_proved_native_member_cannot_spend_independent_configured_admission_groups() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (mut config, _, workspace, native) = setup(root.path(), &fixture, true);
    workspace
        .add_connection(NewConnection {
            name: "peer".into(),
            input: "mssql://reader@fixture.invalid/sample".into(),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    qualify(&workspace, &mut outbox, &fixture, "peer").await;
    let mut peer = config.inventory.targets[0].clone();
    peer.id = "peer".into();
    peer.connection = "peer".into();
    peer.admission_group = "a-second-label-for-the-same-native-member".into();
    config.inventory.targets.push(peer);
    let plan = Enrollment::resolve(&workspace, &config).unwrap();
    assert!(plan.inventory.is_none());
    assert_eq!(plan.summary.gaps.len(), 2);
    assert!(plan.summary.gaps.iter().all(|gap| gap.reason == MonitoringGapReason::SharedAdmissionConflict));
    assert_eq!(native.factories.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn revoked_or_retired_original_approval_cannot_dispatch_an_enrolled_profile() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (config, _, workspace, native) = setup(root.path(), &fixture, true);
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    let plan = Enrollment::resolve(&workspace, &config).unwrap();
    workspace.revoke_recipe_qualification("fixture", &fixture.recipe.id, &outbox.delivery()).await.unwrap();
    let (summary, events) = run_one(
        &config,
        &workspace,
        &plan,
        &mut outbox,
        &root.path().join("fleet"),
        &CollectionCancellation::default(),
    )
    .await;
    assert_eq!(summary.incomplete, 1);
    assert!(events[0].capture.is_none());
    assert_eq!(native.factories.load(Ordering::SeqCst), 0);
    assert_eq!(native.source_queries.load(Ordering::SeqCst), 0);
    assert!(!workspace.source_gate.is_closed());
}

#[tokio::test]
async fn native_unknown_role_and_changed_process_refuse_before_source_queries_and_still_close_the_original_reader() {
    for role in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
        let (config, _, workspace, native) = setup(root.path(), &fixture, true);
        let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
        qualify(&workspace, &mut outbox, &fixture, "fixture").await;
        let plan = Enrollment::resolve(&workspace, &config).unwrap();
        if role {
            native.context.lock().unwrap().database_writable = None;
        } else {
            let mut context = native.context.lock().unwrap();
            let SourcePartition::InstanceLocal { epoch, .. } = &mut context.partition else { panic!() };
            *epoch = Some("restarted-native-process".into());
        }
        let (summary, _) = run_one(
            &config,
            &workspace,
            &plan,
            &mut outbox,
            &root.path().join("fleet"),
            &CollectionCancellation::default(),
        )
        .await;
        assert_eq!(summary.incomplete, 1);
        assert_eq!(native.source_queries.load(Ordering::SeqCst), 0);
        assert!(fixture.closed());
        assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    }
}

#[tokio::test]
async fn unconfirmed_profile_shutdown_fences_future_work_and_cancelled_start_opens_nothing() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (config, _, workspace, native) = setup(root.path(), &fixture, true);
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    let plan = Enrollment::resolve(&workspace, &config).unwrap();
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    let (summary, _) =
        run_one(&config, &workspace, &plan, &mut outbox, &root.path().join("cancelled-fleet"), &cancellation).await;
    assert_eq!(summary.attempts, 0);
    assert_eq!(native.factories.load(Ordering::SeqCst), 0);
    native.uncertain_close.store(true, Ordering::SeqCst);
    let backend = plan.backend(&workspace);
    let inventory = plan.inventory.as_ref().unwrap();
    let journal = FleetJournal::open(&root.path().join("uncertain-fleet"), inventory, &config.shard).await.unwrap();
    let mut runner = FleetRunner::new(&backend, inventory, &config.shard, &journal, &mut outbox).unwrap();
    let error = runner.run(&CollectionCancellation::default(), Some(1), |_| Ok(())).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert!(workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(workspace.source_gate.is_closed());
}

#[tokio::test]
async fn enrollment_summary_contains_only_its_curated_logical_fields_and_explicit_mode_keeps_the_existing_inventory() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (mut config, _, workspace, _) = setup(root.path(), &fixture, true);
    let plan = Enrollment::resolve(&workspace, &config).unwrap();
    let value = serde_json::to_value(&plan.summary).unwrap();
    let keys = value.as_object().unwrap().keys().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(keys, BTreeSet::from(["assigned_targets", "explicit_schedules", "automatic", "gaps", "omitted_gaps"]));
    for gap in value["gaps"].as_array().unwrap() {
        assert_eq!(
            gap.as_object().unwrap().keys().map(String::as_str).collect::<BTreeSet<_>>(),
            BTreeSet::from(["target", "recipe_id", "reason"])
        );
    }
    config.monitoring = MonitoringPolicy::Explicit;
    assert!(config.validate().is_err());
    config.inventory.targets[0].schedules.push(FleetSchedule {
        id: "manual".into(),
        every_seconds: 300,
        jitter_seconds: 0,
        collection: ScheduledCollection::Snapshot { dataset: SnapshotDataset::QueryCompilation },
        budget: fixture.request().budget,
    });
    let explicit = Enrollment::resolve(&workspace, &config).unwrap();
    assert!(explicit.summary.gaps.is_empty());
    assert!(explicit.summary.automatic.is_empty());
    assert_eq!(
        serde_json::to_value(explicit.inventory.unwrap()).unwrap(),
        serde_json::to_value(&config.inventory).unwrap()
    );
}

#[tokio::test]
async fn revocation_between_source_calls_withholds_new_work_but_keeps_the_original_close_owner() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (config, _, workspace, native) = setup(root.path(), &fixture, true);
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    let plan = Enrollment::resolve(&workspace, &config).unwrap();
    let profile = plan.profiles.values().next().unwrap();
    let request = fixture.request();
    let registration = workspace.connection_service().profile_registration("fixture", &request, profile).unwrap();
    let provider = NativeProvider { fixture: fixture.clone() };
    let mut source = registration.profile_source(&provider, &request, profile).unwrap();
    source.describe().await.unwrap();
    assert_eq!(native.source_queries.load(Ordering::SeqCst), 1);
    workspace.revoke_recipe_qualification("fixture", &fixture.recipe.id, &outbox.delivery()).await.unwrap();
    assert!(!profile.approval_is_active());
    assert_eq!(
        CollectionStop::from_error(&source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap_err()),
        CollectionStop::SourceChanged
    );
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(native.source_queries.load(Ordering::SeqCst), 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(fixture.closed());
}

#[tokio::test]
async fn caller_created_liveness_cannot_resurrect_an_approval_after_its_original_owner_retires_it() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let (config, _, workspace, native) = setup(root.path(), &fixture, true);
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    let plan = Enrollment::resolve(&workspace, &config).unwrap();
    let original = plan.profiles.values().next().unwrap();
    let counterfeit = SourceProfileAdmission::new(
        original.route_revision().into(),
        original.qualification().clone(),
        fixture.request().budget,
        || true,
    )
    .unwrap();
    workspace.revoke_recipe_qualification("fixture", &fixture.recipe.id, &outbox.delivery()).await.unwrap();
    assert!(counterfeit.approval_is_active());
    assert!(workspace.connection_service().bound_profile("fixture", &fixture.request(), &counterfeit).is_err());
    assert_eq!(native.factories.load(Ordering::SeqCst), 0);
    assert_eq!(native.source_queries.load(Ordering::SeqCst), 0);
}

#[test]
fn standard_native_intake_cannot_be_declared_for_an_eager_or_unverified_factory_or_an_unregistered_selector() {
    let fixture = QualificationFixture::baseline(QualificationFixtureCase::Approve);
    let unverified = ProviderRegistration::new(
        Arc::new(|_, _| panic!("metadata validation must not invoke factory")),
        ProviderConstructionPolicy::Unverified,
        vec![fixture.recipe.clone()],
    )
    .unwrap();
    assert!(
        unverified
            .with_profile_source(
                vec![fixture.recipe.selector],
                Arc::new(|provider, request, _| provider.telemetry_source(request))
            )
            .is_err()
    );
    let native = native_state(&fixture);
    let declared = registration(&fixture, native.clone(), false);
    assert!(
        declared
            .with_profile_source(
                vec![RecipeSelector::Collection(TelemetryCollection::Activity)],
                Arc::new(|provider, request, _| provider.telemetry_source(request))
            )
            .is_err()
    );
    assert_eq!(native.factories.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_genuine_activity_collection_can_be_issued_restored_and_enrolled_without_a_snapshot_surrogate() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::baseline_activity(QualificationFixtureCase::Approve);
    let (config, runtime, workspace, native) = setup(root.path(), &fixture, true);
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, Default::default()).unwrap();
    qualify(&workspace, &mut outbox, &fixture, "fixture").await;
    let bindings =
        outbox.delivery().issued_qualifications(Default::default(), &CollectionCancellation::default()).await.unwrap();
    assert_eq!(bindings.bindings[0].collection(), TelemetryCollection::Activity);
    assert_eq!(bindings.bindings[0].receipt().purpose, RecipePurpose::Baseline);
    let restored = runtime.workspace(&config.workspace);
    restored
        .restore_qualifications(&outbox.delivery(), Default::default(), &CollectionCancellation::default())
        .await
        .unwrap();
    let plan = Enrollment::resolve(&restored, &config).unwrap();
    assert!(plan.summary.gaps.is_empty());
    assert!(matches!(
        plan.inventory.as_ref().unwrap().targets[0].schedules[0].collection,
        ScheduledCollection::Activity { .. }
    ));
    let (summary, events) =
        run_one(&config, &restored, &plan, &mut outbox, &root.path().join("fleet"), &CollectionCancellation::default())
            .await;
    assert_eq!((summary.complete, summary.incomplete), (1, 0));
    let attempt = outbox.attempt_receipt(events[0].capture.as_ref().unwrap(), None).await.unwrap();
    let original = outbox
        .delivery()
        .completed_qualification_for_connection(
            "fixture",
            &attempt,
            TelemetryCollection::Activity,
            Default::default(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert!(original.source().observation.activity().is_some());
    assert!(original.source().observation.snapshot().is_none());
    assert_eq!(original.records().len(), fixture.source.observation.activity().unwrap().observations as usize);
    assert_eq!(native.source_queries.load(Ordering::SeqCst), 2);
    assert!(fixture.closed());
}
