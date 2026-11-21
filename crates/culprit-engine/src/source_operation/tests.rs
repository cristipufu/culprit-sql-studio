use super::*;
use crate::{NewConnection, Workspace};
use async_trait::async_trait;
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{
    AuthKind, Engine, Error, ErrorCode, ExplainMode, LiveState, Plan, Provider, ResultSet, ServerInfo, Snapshot,
    SnapshotOptions,
    telemetry::{CollectionRequest, CollectionSelection, SnapshotDataset},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Default)]
struct State {
    factories: AtomicUsize,
    enrichments: AtomicUsize,
    uncertain: AtomicBool,
    stop_snapshot: AtomicBool,
    poison_snapshot: AtomicBool,
    poison_enrichment: AtomicBool,
    live_timeout: AtomicBool,
    malformed_discovery: AtomicBool,
    stop_discovery: AtomicBool,
    entered: tokio::sync::Notify,
}
struct FixtureProvider(Arc<State>);
#[async_trait]
impl Provider for FixtureProvider {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }
    fn source_shutdown(&self) -> SourceShutdown {
        if self.0.uncertain.load(Ordering::Acquire) { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent }
    }
    async fn telemetry_intervals(
        &self,
        _: &culprit_core::telemetry::IntervalDiscoveryRequest,
    ) -> Result<culprit_core::telemetry::IntervalDiscoveryPage> {
        self.0.entered.notify_one();
        if self.0.stop_discovery.load(Ordering::Acquire) {
            std::future::pending::<()>().await;
        }
        let from = if self.0.malformed_discovery.load(Ordering::Acquire) {
            "2026-11-10T07:00:00Z"
        } else {
            "2026-11-10T06:00:00Z"
        };
        Ok(culprit_core::telemetry::IntervalDiscoveryPage {
            partition: culprit_core::telemetry::SourcePartition::Target,
            database: culprit_core::DatabaseIdentity::server(Engine::Postgres, "db", "fixture.invalid", 5432, None),
            source: "native-history".into(),
            observed_at: "2026-11-10T08:01:00Z".into(),
            metadata_from: None,
            intervals: vec![culprit_core::telemetry::TelemetryInterval {
                native_id: "native-interval-1".into(),
                from: from.into(),
                until: "2026-11-10T07:00:00Z".into(),
            }],
            next_cursor: None,
        })
    }
    async fn snapshot(&self, _: &str, _: &SnapshotOptions) -> Result<Snapshot> {
        self.0.entered.notify_one();
        if self.0.stop_snapshot.load(Ordering::Acquire) {
            std::future::pending::<()>().await;
        }
        self.0.uncertain.store(self.0.poison_snapshot.load(Ordering::Acquire), Ordering::Release);
        Ok(serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap())
    }
    async fn recorded_changes(&self, _: &str) -> Result<Vec<culprit_core::ChangeEvent>> {
        self.0.enrichments.fetch_add(1, Ordering::Relaxed);
        if self.0.poison_enrichment.load(Ordering::Acquire) {
            self.0.uncertain.store(true, Ordering::Release);
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        Ok(vec![])
    }
    async fn probe(&self) -> Result<ServerInfo> {
        Err(unused())
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        Err(unused())
    }
    async fn live(&self) -> Result<LiveState> {
        if self.0.live_timeout.load(Ordering::Relaxed) {
            return Err(Error::new(ErrorCode::Timeout, "unconfirmed fixture timeout"));
        }
        Err(unused())
    }
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(unused())
    }
}
fn unused() -> Error {
    Error::new(ErrorCode::Unsupported, "unused fixture operation")
}
fn workspace(home: &std::path::Path, state: &Arc<State>) -> Workspace {
    let state = state.clone();
    Workspace::open_in(home).with_provider(
        Engine::Postgres,
        crate::ProviderRegistration::new(
            Arc::new(move |_, _context| {
                state.factories.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(FixtureProvider(state.clone())))
            }),
            crate::ProviderConstructionPolicy::DeferredNativeWork,
            Vec::new(),
        )
        .unwrap(),
    )
}
fn configured(home: &std::path::Path, state: &Arc<State>) -> Workspace {
    let workspace = workspace(home, state);
    workspace
        .add_connection(NewConnection {
            name: "pilot".into(),
            input: "postgres://reader@fixture.invalid/demo".into(),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    workspace
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: Default::default(),
    }
}

fn discovery_request() -> culprit_core::telemetry::IntervalDiscoveryRequest {
    culprit_core::telemetry::IntervalDiscoveryRequest {
        from: "2026-11-10T06:30:00Z".into(),
        until: "2026-11-10T08:00:00Z".into(),
        cursor: None,
        limit: 2,
        budget: Default::default(),
    }
}

#[tokio::test(start_paused = true)]
async fn admitted_discovery_times_out_on_an_occupied_permit_without_authentication_or_clearing_other_work() {
    struct NoAuthentication(AtomicUsize);
    #[async_trait]
    impl culprit_auth::DatabaseAuthentication for NoAuthentication {
        fn auth_kind(&self) -> AuthKind {
            AuthKind::Entra
        }
        async fn credential(
            &self,
            _: &culprit_core::ConnectionSpec,
            _: &dyn culprit_auth::SecretStore,
        ) -> Result<culprit_auth::DatabaseCredential> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(unused())
        }
    }
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let authentication = Arc::new(NoAuthentication(AtomicUsize::new(0)));
    let workspace = configured(home.path(), &state).with_authentication_provider(authentication.clone());
    workspace
        .store
        .update("pilot", |stored| {
            stored.auth = AuthKind::Entra;
            stored.host = "fixture.postgres.database.azure.com".into();
        })
        .unwrap();
    let mut occupied = workspace.source_gate.acquire().await.unwrap();
    occupied.begin().unwrap();
    let admitted = culprit_collector::fleet::FleetSourceAdmission {
        window: culprit_collector::capture::CollectionAdmissionWindow::new(
            tokio::time::Instant::now(),
            &Default::default(),
        )
        .unwrap(),
        route_revision: Some(workspace.connection_service().route_revision("pilot").unwrap()),
        investigation: None,
    };
    let error = workspace.discover_intervals_admitted("pilot", &discovery_request(), &admitted).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert!(tokio::time::Instant::now() >= admitted.window.start_before());
    assert_eq!(authentication.0.load(Ordering::SeqCst), 0);
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert!(
        !workspace.source_gate.is_closed(),
        "timing out a ready permit waiter cannot settle or fence its current owner"
    );
    assert!(
        !workspace.collection_admission_status().await.unwrap().source_quiescence_required,
        "the waiter did not start native admission"
    );
    occupied.settle(SourceShutdown::Quiescent).unwrap();
    drop(occupied);
    drop(workspace.source_gate.acquire().await.unwrap());

    // A separate persisted debt is refused, never cleared by a new admitted operation's setup failure.
    let journal = SourceAdmission::acquire(workspace.collection_admission_path()).await.unwrap();
    let original = culprit_read::admission::observation_identity().unwrap();
    journal
        .started(
            original.observation_id.clone(),
            culprit_core::telemetry::SourceAdmissionRoute::SavedConnection { name: "pilot".into() },
        )
        .await
        .unwrap();
    drop(journal);
    let fresh = culprit_collector::fleet::FleetSourceAdmission {
        window: culprit_collector::capture::CollectionAdmissionWindow::new(
            tokio::time::Instant::now(),
            &Default::default(),
        )
        .unwrap(),
        route_revision: Some(workspace.connection_service().route_revision("pilot").unwrap()),
        investigation: None,
    };
    let error = workspace.discover_intervals_admitted("pilot", &discovery_request(), &fresh).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    let debt = workspace.collection_admission_status().await.unwrap();
    assert!(debt.source_quiescence_required);
    assert_eq!(debt.observation_id.as_deref(), Some(original.observation_id.as_str()));
    assert_eq!(authentication.0.load(Ordering::SeqCst), 0);
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn discovery_validates_before_authentication_and_clears_ownership_only_after_a_valid_native_result() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let workspace = configured(home.path(), &state);
    let mut request = discovery_request();
    request.limit = 0;
    assert!(workspace.discover_intervals("pilot", &request).await.is_err());
    assert_eq!(state.factories.load(Ordering::Relaxed), 0);
    request.limit = 2;
    let page = workspace.discover_intervals("pilot", &request).await.unwrap();
    assert_eq!(page.intervals[0].from, "2026-11-10T06:00:00Z");
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    state.malformed_discovery.store(true, Ordering::Release);
    let error = workspace.discover_intervals("pilot", &request).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
}
#[tokio::test]
async fn discovery_cannot_return_success_or_admit_a_scan_after_unknown_native_shutdown() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let workspace = configured(home.path(), &state);
    state.uncertain.store(true, Ordering::Release);
    let error = workspace.discover_intervals("pilot", &discovery_request()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert!(
        crate::tests::observe_provider(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| workspace
            .scan("pilot", &SnapshotOptions::default(), operation)
            .await)
        .await
        .is_err()
    );
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn dropping_discovery_preserves_the_same_durable_native_debt_as_a_dropped_scan() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.stop_discovery.store(true, Ordering::Release);
    let workspace = Arc::new(configured(home.path(), &state));
    let reader = workspace.clone();
    let task = tokio::spawn(async move { reader.discover_intervals("pilot", &discovery_request()).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), state.entered.notified()).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(workspace.discover_intervals("pilot", &discovery_request()).await.is_err());
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn uncertain_primary_scan_stops_enrichment_and_blocks_capture_before_and_after_restart() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.poison_snapshot.store(true, Ordering::Relaxed);
    let ws = configured(home.path(), &state);
    let result = crate::tests::observe_provider(&ws, culprit_core::ProviderReadKind::Scan, async |operation| {
        ws.scan("pilot", &SnapshotOptions::default(), operation).await
    })
    .await;
    assert_eq!(CollectionStop::from_error(&result.err().unwrap()), CollectionStop::CancellationUnconfirmed);
    assert_eq!(state.enrichments.load(Ordering::Relaxed), 0);
    assert!(ws.latest_report("pilot", None).unwrap().is_none());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
    let capture = home.path().join("capture");
    assert!(ws.collect_once("pilot", request(), &capture, &CollectionCancellation::default()).await.is_err());
    assert!(!capture.exists());
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
    drop(ws);
    let restarted = workspace(home.path(), &state);
    assert!(
        crate::tests::observe_provider(&restarted, culprit_core::ProviderReadKind::Scan, async |operation| restarted
            .scan("pilot", &SnapshotOptions::default(), operation)
            .await)
        .await
        .is_err()
    );
    assert!(restarted.collect_once("pilot", request(), &capture, &CollectionCancellation::default()).await.is_err());
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn retiring_a_scan_future_retains_native_debt_and_blocks_a_replacement_factory() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.stop_snapshot.store(true, Ordering::Relaxed);
    let ws = Arc::new(configured(home.path(), &state));
    let task_ws = ws.clone();
    let task = tokio::spawn(async move {
        crate::tests::observe_provider(&task_ws, culprit_core::ProviderReadKind::Scan, async |operation| {
            task_ws.scan("pilot", &SnapshotOptions::default(), operation).await
        })
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), state.entered.notified()).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(ws.source_gate.is_closed());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(
        crate::tests::observe_provider(&ws, culprit_core::ProviderReadKind::Scan, async |operation| ws
            .scan("pilot", &SnapshotOptions::default(), operation)
            .await)
        .await
        .is_err()
    );
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn uncertain_enrichment_cannot_save_a_successful_report_or_clear_source_ownership() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.poison_enrichment.store(true, Ordering::Relaxed);
    let ws = configured(home.path(), &state);
    assert!(
        crate::tests::observe_provider(&ws, culprit_core::ProviderReadKind::Scan, async |operation| ws
            .scan("pilot", &SnapshotOptions::default(), operation)
            .await)
        .await
        .is_err()
    );
    assert!(ws.latest_report("pilot", None).unwrap().is_none());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
}
#[tokio::test]
async fn a_confirmed_scan_releases_admission_before_offline_report_work() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = configured(home.path(), &state);
    let result = crate::tests::observe_provider(&ws, culprit_core::ProviderReadKind::Scan, async |operation| {
        ws.scan("pilot", &SnapshotOptions::default(), operation).await
    })
    .await
    .unwrap();
    assert!(!result.snapshot.tables.is_empty());
    assert!(!ws.source_gate.is_closed());
    assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
    crate::tests::observe_provider(&ws, culprit_core::ProviderReadKind::Scan, async |operation| {
        ws.scan("pilot", &SnapshotOptions::default(), operation).await
    })
    .await
    .unwrap();
    assert_eq!(state.factories.load(Ordering::Relaxed), 2);
}

struct BlockingAnalysis {
    entered: tokio::sync::Notify,
    released: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
}
impl culprit_core::Check for BlockingAnalysis {
    fn info(&self) -> culprit_core::CheckInfo {
        culprit_core::CheckInfo {
            id: "test.offline-analysis".into(),
            title: "offline fixture".into(),
            description: "blocks offline analysis".into(),
            category: culprit_core::Category::Configuration,
            engines: vec![Engine::Postgres],
            requires: vec![],
        }
    }
    fn run(&self, _: &Snapshot) -> Vec<culprit_core::Finding> {
        self.entered.notify_one();
        let release = self.released.lock().unwrap();
        let _ =
            self.wake.wait_timeout_while(release, std::time::Duration::from_secs(5), |released| !*released).unwrap();
        vec![]
    }
}
#[tokio::test]
async fn offline_analysis_neither_holds_native_admission_nor_opens_database_sessions() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let analysis = Arc::new(BlockingAnalysis {
        entered: tokio::sync::Notify::new(),
        released: std::sync::Mutex::new(false),
        wake: std::sync::Condvar::new(),
    });
    let ws = Arc::new(configured(home.path(), &state).with_check(analysis.clone()));
    let scanning = ws.clone();
    let task = tokio::spawn(async move {
        crate::tests::observe_provider(&scanning, culprit_core::ProviderReadKind::Scan, async |operation| {
            scanning.scan("pilot", &SnapshotOptions::default(), operation).await
        })
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), analysis.entered.notified()).await.unwrap();
    assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
    let error = ws
        .collect_once("pilot", request(), &home.path().join("during-analysis"), &CollectionCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::Unsupported,
        "a fresh factory is reached; the completed scan no longer owns native admission"
    );
    assert_eq!(state.factories.load(Ordering::Relaxed), 2);
    *analysis.released.lock().unwrap() = true;
    analysis.wake.notify_all();
    task.await.unwrap().unwrap();
    assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn unsettled_scan_also_blocks_automatic_live_probe_and_progress_before_provider_creation() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.poison_snapshot.store(true, Ordering::Relaxed);
    let ws = configured(home.path(), &state);
    assert!(
        crate::tests::observe_provider(&ws, culprit_core::ProviderReadKind::Scan, async |operation| ws
            .scan("pilot", &SnapshotOptions::default(), operation)
            .await)
        .await
        .is_err()
    );
    assert!(ws.live("pilot").await.is_err());
    assert!(
        crate::tests::observe_provider(&ws, culprit_core::ProviderReadKind::Probe, async |operation| ws
            .probe("pilot", operation)
            .await)
        .await
        .is_err()
    );
    assert!(ws.query_progress("pilot", "42", false).await.is_err());
    assert!(ws.wait_sample("pilot", 1).await.is_err());
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn an_untyped_observation_timeout_cannot_clear_native_ownership() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.live_timeout.store(true, Ordering::Relaxed);
    let ws = configured(home.path(), &state);
    let error = ws.live("pilot").await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(
        crate::tests::observe_provider(&ws, culprit_core::ProviderReadKind::Scan, async |operation| ws
            .scan("pilot", &SnapshotOptions::default(), operation)
            .await)
        .await
        .is_err()
    );
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn all_normal_probe_route_kinds_use_the_same_original_journal_and_gate() {
    use culprit_core::{
        ProviderReadKind, ProviderReadObservation,
        telemetry::{ConnectionTestKind, SourceAdmissionRoute},
    };
    for variant in 0..4 {
        let home = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let ws = configured(home.path(), &state);
        let id = culprit_read::admission::observation_identity().unwrap().observation_id;
        let owner = ProviderReadObservation::begin(id.clone(), ProviderReadKind::Probe).unwrap();
        let association = match variant {
            0 => SourceAdmissionRoute::SavedConnection { name: "pilot".into() },
            1 => SourceAdmissionRoute::UnsavedConnectionTest { invocation_id: id.clone() },
            2 => SourceAdmissionRoute::SavedConnectionTest { name: "pilot".into(), test: ConnectionTestKind::Settings },
            _ => SourceAdmissionRoute::SavedConnectionTest { name: "pilot".into(), test: ConnectionTestKind::Ssh },
        };
        let operation = ws
            .connection_service()
            .source_operation_at(association.clone(), None, owner.operation().unwrap())
            .await
            .unwrap();
        let started = ws.collection_admission_status().await.unwrap();
        assert_eq!(started.route, Some(association.clone()));
        assert_eq!(started.observation_id, Some(id));
        assert!(started.source_quiescence_required);
        let attempt = owner.finish_call(Ok(operation.finish_call(Ok(42), SourceShutdown::Quiescent).await));
        assert_eq!(attempt.acquisition().route, Some(association));
        assert_eq!(attempt.acquisition().native_disposition, culprit_core::ProviderReadDisposition::NotStarted);
        assert_eq!(attempt.into_result().unwrap(), 42);
        assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
        assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn canceled_normal_probe_owner_cannot_claim_quiet_from_a_finished_reader_snapshot() {
    use culprit_core::{ProviderReadKind, ProviderReadObservation, telemetry::SourceAdmissionRoute};
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = configured(home.path(), &state);
    let id = culprit_read::admission::observation_identity().unwrap().observation_id;
    let owner = ProviderReadObservation::begin(id, ProviderReadKind::Probe).unwrap();
    let context = owner.context();
    let mut operation = ws
        .connection_service()
        .source_operation_at(
            SourceAdmissionRoute::SavedConnection { name: "pilot".into() },
            None,
            owner.operation().unwrap(),
        )
        .await
        .unwrap();
    operation.native_started();
    let reader = context.reader(Engine::Postgres);
    reader.finish(SourceShutdown::Quiescent);
    drop(operation);
    let attempt = owner.finish::<()>(Err(CollectionStop::Cancelled.error()));
    assert_eq!(attempt.acquisition().native_disposition, culprit_core::ProviderReadDisposition::Unconfirmed);
    assert!(ws.source_gate.is_closed());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert_eq!(
        CollectionStop::from_error(&attempt.into_result().unwrap_err()),
        CollectionStop::CancellationUnconfirmed
    );
}
#[tokio::test]
async fn normal_probe_retains_original_failure_when_the_real_owner_refuses_shutdown() {
    use culprit_core::{ProviderReadKind, ProviderReadObservation, telemetry::SourceAdmissionRoute};
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = configured(home.path(), &state);
    let id = culprit_read::admission::observation_identity().unwrap().observation_id;
    let owner = ProviderReadObservation::begin(id, ProviderReadKind::Probe).unwrap();
    let mut operation = ws
        .connection_service()
        .source_operation_at(
            SourceAdmissionRoute::SavedConnection { name: "pilot".into() },
            None,
            owner.operation().unwrap(),
        )
        .await
        .unwrap();
    operation.native_started();
    let original = CollectionStop::ByteLimit.error().with_hint("private original error hint");
    let expected = serde_json::to_value(&original).unwrap();
    let attempt = owner.finish_call(Ok(operation.finish_call::<()>(Err(original), SourceShutdown::Unconfirmed).await));
    assert_eq!(
        serde_json::to_value(attempt.original().expect("materialized original result").as_ref().unwrap_err()).unwrap(),
        expected
    );
    assert_eq!(attempt.acquisition().original_failure.as_ref().unwrap().stop, Some(CollectionStop::ByteLimit));
    assert_eq!(
        attempt.acquisition().finalization_failure.as_ref().unwrap().stop,
        Some(CollectionStop::CancellationUnconfirmed)
    );
    assert!(ws.source_gate.is_closed());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
}
#[tokio::test]
async fn successful_native_probe_value_is_retained_when_durable_finalization_fails() {
    use culprit_core::{ProviderReadKind, ProviderReadObservation, telemetry::SourceAdmissionRoute};
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = configured(home.path(), &state);
    let id = culprit_read::admission::observation_identity().unwrap().observation_id;
    let owner = ProviderReadObservation::begin(id, ProviderReadKind::Probe).unwrap();
    let mut operation = ws
        .connection_service()
        .source_operation_at(
            SourceAdmissionRoute::SavedConnection { name: "pilot".into() },
            None,
            owner.operation().unwrap(),
        )
        .await
        .unwrap();
    operation.native_started();
    std::fs::create_dir(ws.collection_admission_path().join(".state.json.new")).unwrap();
    let attempt = owner.finish_call(Ok(operation.finish_call(Ok(42), SourceShutdown::Quiescent).await));
    assert_eq!(attempt.original().expect("materialized original result").as_ref().unwrap(), &42);
    assert_eq!(attempt.acquisition().native_disposition, culprit_core::ProviderReadDisposition::Quiescent);
    assert_eq!(attempt.acquisition().original_failure, None);
    assert_eq!(attempt.acquisition().finalization_failure.as_ref().unwrap().stop, Some(CollectionStop::StorageFailed));
    assert!(!ws.source_gate.is_closed());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert_eq!(CollectionStop::from_error(&attempt.into_result().unwrap_err()), CollectionStop::StorageFailed);
}
