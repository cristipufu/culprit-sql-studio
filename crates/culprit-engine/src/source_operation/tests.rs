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
            Arc::new(move |_| {
                state.factories.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(FixtureProvider(state.clone())))
            }),
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
    assert!(workspace.scan("pilot", &SnapshotOptions::default()).await.is_err());
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
    let result = ws.scan("pilot", &SnapshotOptions::default()).await;
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
    assert!(restarted.scan("pilot", &SnapshotOptions::default()).await.is_err());
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
    let task = tokio::spawn(async move { task_ws.scan("pilot", &SnapshotOptions::default()).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), state.entered.notified()).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(ws.source_gate.is_closed());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(ws.scan("pilot", &SnapshotOptions::default()).await.is_err());
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn uncertain_enrichment_cannot_save_a_successful_report_or_clear_source_ownership() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.poison_enrichment.store(true, Ordering::Relaxed);
    let ws = configured(home.path(), &state);
    assert!(ws.scan("pilot", &SnapshotOptions::default()).await.is_err());
    assert!(ws.latest_report("pilot", None).unwrap().is_none());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
}
#[tokio::test]
async fn a_confirmed_scan_releases_admission_before_offline_report_work() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = configured(home.path(), &state);
    let result = ws.scan("pilot", &SnapshotOptions::default()).await.unwrap();
    assert!(!result.snapshot.tables.is_empty());
    assert!(!ws.source_gate.is_closed());
    assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
    ws.scan("pilot", &SnapshotOptions::default()).await.unwrap();
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
    let task = tokio::spawn(async move { scanning.scan("pilot", &SnapshotOptions::default()).await });
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
    assert!(ws.scan("pilot", &SnapshotOptions::default()).await.is_err());
    assert!(ws.live("pilot").await.is_err());
    assert!(ws.probe("pilot").await.is_err());
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
    assert!(ws.scan("pilot", &SnapshotOptions::default()).await.is_err());
    assert_eq!(state.factories.load(Ordering::Relaxed), 1);
}
