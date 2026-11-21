//! Controlled reader/factory lifecycle only; these counters establish no native identity or cost proof.
use super::*;
use crate::{ConnectionSettings, NewConnection, ProviderConstructionPolicy, ProviderRegistration, Workspace};
use async_trait::async_trait;
use culprit_core::{
    AuthKind, Engine, Error, ErrorCode, ExplainMode, LiveState, Plan, Provider, ProviderAcquisitionReport,
    ProviderReadContext, ProviderReadKind, Result, ResultSet, Snapshot, SnapshotOptions, telemetry::*,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Default)]
struct State {
    factories: AtomicUsize,
    mode: AtomicUsize,
    entered: tokio::sync::Notify,
    fail_finalization: std::sync::Mutex<Option<std::path::PathBuf>>,
}
struct FixtureProvider {
    state: Arc<State>,
    context: ProviderReadContext,
}
fn failure() -> Error {
    Error::new(ErrorCode::QueryFailed, "private-original-error")
        .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::ByteLimit))
}
#[async_trait]
impl Provider for FixtureProvider {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }
    fn source_shutdown(&self) -> SourceShutdown {
        if self.state.mode.load(Ordering::Acquire) == 2 {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        }
    }
    async fn probe(&self) -> Result<ServerInfo> {
        let reader = self.context.reader(Engine::Postgres);
        reader.setup_included();
        let metrics = ReadMetricsRecorder::new(Engine::Postgres);
        let template = metrics.begin("SELECT private_fixture_source");
        reader.checkpoint(1, metrics.snapshot());
        if self.state.mode.load(Ordering::Acquire) == 4 {
            panic!("controlled fixture provider panic");
        }
        if self.state.mode.load(Ordering::Acquire) == 3 {
            self.state.entered.notify_one();
            std::future::pending::<()>().await;
        }
        let failed = self.state.mode.load(Ordering::Acquire) > 0;
        metrics.finish(
            template.as_deref(),
            std::time::Duration::from_millis(1),
            &ReadProgress::default(),
            &if failed { ReadTaskOutcome::stopped(CollectionStop::ByteLimit) } else { ReadTaskOutcome::default() },
        );
        reader.checkpoint(1, metrics.snapshot());
        reader.finish(SourceShutdown::Quiescent);
        if failed {
            return Err(failure());
        }
        let snapshot: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
        Ok(snapshot.server)
    }
    async fn snapshot(&self, name: &str, _: &SnapshotOptions) -> Result<Snapshot> {
        self.probe().await?;
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
        snapshot.connection = name.into();
        Ok(snapshot)
    }
    async fn recorded_changes(&self, _: &str) -> Result<Vec<culprit_core::ChangeEvent>> {
        if let Some(path) = self.state.fail_finalization.lock().unwrap().take() {
            std::fs::create_dir(path).unwrap();
        }
        Ok(vec![])
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        Err(Error::internal("unused"))
    }
    async fn live(&self) -> Result<LiveState> {
        Err(Error::internal("unused"))
    }
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(Error::internal("unused"))
    }
}
fn workspace(path: &std::path::Path, state: &Arc<State>, policy: ProviderConstructionPolicy) -> Workspace {
    let state = state.clone();
    Workspace::open_in(path).with_provider(
        Engine::Postgres,
        ProviderRegistration::new(
            Arc::new(move |_, context| {
                assert!(context.is_observed(), "context reaches the factory before any provider method");
                context.complete_reader_coverage();
                state.factories.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(FixtureProvider { state: state.clone(), context }))
            }),
            policy,
            vec![],
        )
        .unwrap(),
    )
}
fn input() -> NewConnection {
    NewConnection {
        name: "unsaved-display-label".into(),
        input: "postgres://reader@fixture.invalid/demo".into(),
        auth: Some(AuthKind::None),
        ..Default::default()
    }
}
fn saved(path: &std::path::Path, state: &Arc<State>) -> Workspace {
    let ws = workspace(path, state, ProviderConstructionPolicy::DeferredNativeWork);
    let mut new = input();
    new.name = "pilot".into();
    ws.add_connection(new).unwrap();
    ws
}

#[tokio::test]
async fn unsaved_probe_uses_one_actual_admission_without_inventing_a_saved_name() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = workspace(home.path(), &state, ProviderConstructionPolicy::DeferredNativeWork);
    let owner = ws.begin_provider_read(ProviderReadKind::Probe).unwrap();
    let result = ws.test_connection(&input(), owner.operation().unwrap()).await;
    let attempt = owner.finish_call(result);
    assert!(attempt.acquisition().is_valid());
    assert!(matches!(attempt.acquisition().route.as_ref(), Some(SourceAdmissionRoute::UnsavedConnectionTest { .. })));
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::Quiescent);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert!(ws.connections().unwrap().is_empty());
    assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(project_probe_attempt(attempt).require_connected().is_ok());
}
#[tokio::test]
async fn unverified_factory_is_refused_before_it_can_start_work_and_leaves_a_not_started_result() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = workspace(home.path(), &state, ProviderConstructionPolicy::Unverified);
    let owner = ws.begin_provider_read(ProviderReadKind::Probe).unwrap();
    let result = ws.test_connection(&input(), owner.operation().unwrap()).await;
    let attempt = owner.finish_call(result);
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::NotStarted);
    assert_eq!(
        attempt.original().expect("materialized original result").as_ref().unwrap_err().code,
        ErrorCode::Unsupported
    );
    assert!(attempt.finalization().is_ok());
    assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
}
#[tokio::test]
async fn original_error_detail_survives_a_distinct_actual_unconfirmed_finalization() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.mode.store(2, Ordering::Release);
    let ws = saved(home.path(), &state);
    let owner = ws.begin_provider_read(ProviderReadKind::Probe).unwrap();
    let result = ws.probe("pilot", owner.operation().unwrap()).await;
    let attempt = owner.finish_call(result);
    assert_eq!(
        serde_json::to_value(attempt.original().expect("materialized original result").as_ref().unwrap_err()).unwrap(),
        serde_json::to_value(failure()).unwrap()
    );
    assert_eq!(
        CollectionStop::from_error(attempt.finalization().as_ref().unwrap_err()),
        CollectionStop::CancellationUnconfirmed
    );
    let projected = project_probe_attempt(attempt);
    assert!(projected.require_connected().is_err());
    assert!(matches!(
        projected.outcome,
        ProbeOutcome::NativeWorkUnconfirmed { original: OriginalProbeResult::Failed { .. }, .. }
    ));
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
}
#[tokio::test]
async fn canceled_probe_keeps_its_exact_owner_and_cannot_admit_a_replacement_factory() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.mode.store(3, Ordering::Release);
    let ws = saved(home.path(), &state);
    let owner = ws.begin_provider_read(ProviderReadKind::Probe).unwrap();
    let mut work = Box::pin(ws.probe("pilot", owner.operation().unwrap()));
    tokio::select! {_ = state.entered.notified()=>(),_=&mut work=>panic!("pending original probe")};
    drop(work);
    let attempt = owner.finish_call::<ServerInfo>(Err(CollectionStop::Cancelled.error()));
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::Unconfirmed);
    let replacement = ws.begin_provider_read(ProviderReadKind::Probe).unwrap();
    let result = ws.probe("pilot", replacement.operation().unwrap()).await;
    assert!(replacement.finish_call(result).into_result().is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
}
#[tokio::test]
async fn settings_and_ssh_tests_preserve_the_saved_route_kind_under_the_same_gate() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = saved(home.path(), &state);
    for test in [ConnectionTestKind::Settings, ConnectionTestKind::Ssh] {
        let owner = ws.begin_provider_read(ProviderReadKind::Probe).unwrap();
        let settings = ConnectionSettings {
            environment: culprit_core::Environment::Local,
            security: ws.connection("pilot").unwrap().security.unwrap().settings,
        };
        let result = match test {
            ConnectionTestKind::Settings => {
                ws.test_connection_settings("pilot", &settings, owner.operation().unwrap()).await
            }
            ConnectionTestKind::Ssh => ws.test_ssh("pilot", None, owner.operation().unwrap()).await,
        };
        let attempt = owner.finish_call(result);
        assert_eq!(
            attempt.acquisition().route,
            Some(SourceAdmissionRoute::SavedConnectionTest { name: "pilot".into(), test })
        );
        assert!(attempt.into_result().is_ok());
        assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
    }
    assert_eq!(state.factories.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn saved_scan_rereads_current_accounting_source_free_and_whole_input_sharing_withdraws_it() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = saved(home.path(), &state);
    let owner = ws.begin_provider_read(ProviderReadKind::Scan).unwrap();
    let result = ws.scan("pilot", &SnapshotOptions::default(), owner.operation().unwrap()).await;
    let attempt = owner.finish_call(result);
    let expected = attempt.acquisition().clone();
    let report = attempt.into_result().unwrap();
    assert!(matches!(report.provider_acquisition.as_deref(), Some(ProviderAcquisitionReport::Available { .. })));
    let displayed = serde_json::to_value(crate::reports::display(&report)).unwrap();
    assert_eq!(displayed["provider_acquisition"], serde_json::to_value(&report.provider_acquisition).unwrap());
    assert!(displayed.get("evidence").is_none(), "display keeps the curated report without replay bodies");
    let saved = ws.latest_report("pilot", None).unwrap().unwrap();
    let Some(culprit_core::RecordedProviderAcquisition::Available { summary }) =
        saved.snapshot.operations.provider_acquisition.as_deref()
    else {
        panic!("saved original")
    };
    assert_eq!(summary.as_ref(), &expected);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    let shared = culprit_core::redact::redact_snapshot(&saved.snapshot);
    assert!(matches!(
        shared.operations.provider_acquisition.as_deref(),
        Some(culprit_core::RecordedProviderAcquisition::Unavailable { .. })
    ));
    assert!(!serde_json::to_string(&shared).unwrap().contains(&expected.observation_id));
}

#[tokio::test]
async fn a_probe_capability_cannot_label_or_start_a_scan_and_a_scan_capability_cannot_start_a_probe() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = saved(home.path(), &state);
    let owner = ws.begin_provider_read(ProviderReadKind::Probe).unwrap();
    let result = ws.scan("pilot", &SnapshotOptions::default(), owner.operation().unwrap()).await;
    let attempt = owner.finish_call(result);
    assert!(attempt.into_result().is_err());
    let owner = ws.begin_provider_read(ProviderReadKind::Scan).unwrap();
    let result = ws.probe("pilot", owner.operation().unwrap()).await;
    let attempt = owner.finish_call(result);
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::NotStarted);
    assert!(attempt.into_result().is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert!(!ws.collection_admission_status().await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn unwinding_the_actual_provider_future_keeps_native_debt_without_publishing_a_success() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    state.mode.store(4, Ordering::Release);
    let ws = Arc::new(saved(home.path(), &state));
    let owner = ws.begin_provider_read(ProviderReadKind::Probe).unwrap();
    let operation = owner.operation().unwrap();
    let task_workspace = ws.clone();
    let task = tokio::spawn(async move { task_workspace.probe("pilot", operation).await });
    let Err(error) = task.await else { panic!("fixture must panic before returning a call") };
    assert!(error.is_panic());
    let attempt = owner.finish_call::<ServerInfo>(Err(Error::internal("controlled caller panic boundary")));
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::Unconfirmed);
    assert!(attempt.into_result().is_err());
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert!(ws.report_history("pilot").unwrap().is_empty());
}

#[tokio::test]
async fn successful_scan_acquisition_with_failed_journal_finalization_has_no_original_failure_or_saved_report() {
    let home = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let ws = saved(home.path(), &state);
    *state.fail_finalization.lock().unwrap() = Some(ws.collection_admission_path().join(".state.json.new"));
    let owner = ws.begin_provider_read(ProviderReadKind::Scan).unwrap();
    let result = ws.scan("pilot", &SnapshotOptions::default(), owner.operation().unwrap()).await;
    let attempt = owner.finish_call(result);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert!(attempt.original().is_none());
    assert!(attempt.original_failure().is_none());
    assert_eq!(attempt.acquisition().original_failure, None);
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::Quiescent);
    assert_eq!(attempt.acquisition().finalization_failure.as_ref().unwrap().stop, Some(CollectionStop::StorageFailed));
    assert!(!ws.source_gate.is_closed(), "actual native owner is quiet");
    assert!(
        ws.collection_admission_status().await.unwrap().source_quiescence_required,
        "failed journal confirmation retains durable debt"
    );
    assert!(ws.report_history("pilot").unwrap().is_empty());
    let projected = project_scan_attempt(attempt);
    let culprit_core::ObservedScanAttempt::FinalizationFailed { original_failure, error, acquisition } = projected
    else {
        panic!("finalization-only failure")
    };
    assert!(original_failure.is_none());
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::StorageFailed);
    let ProviderAcquisitionReport::Available { evidence, .. } = acquisition else {
        panic!("original acquisition retained")
    };
    assert_eq!(evidence.original_failure, None);
    assert_eq!(evidence.finalization_failure.as_ref().unwrap().stop, Some(CollectionStop::StorageFailed));
    assert!(ws.report_history("pilot").unwrap().is_empty());
}
