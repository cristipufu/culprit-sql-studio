use super::*;
use crate::{NewConnection, ProviderRegistration};
use culprit_auth::{DatabaseAuthentication, DatabaseCredential};
use culprit_collector::{capture::CollectionAdmissionWindow, fleet::FleetSourceAdmission};
use culprit_core::{
    AuthKind, ConnectionSpec, Environment, Error, ExplainMode, LiveState, Plan, Provider, ResultSet, ServerInfo,
    Snapshot, SnapshotOptions, SshConfig,
};
use std::{collections::BTreeMap, sync::Mutex, time::Duration};
use tokio::{sync::Notify, time::Instant};
use zeroize::Zeroizing;

#[derive(Clone, Debug, PartialEq, Eq)]
struct SavedRoute {
    engine: Option<Engine>,
    host: String,
    port: Option<u16>,
    database: Option<String>,
    user: Option<String>,
    auth: AuthKind,
    options: BTreeMap<String, String>,
    ssh: Option<SshConfig>,
}
impl From<&ConnectionSpec> for SavedRoute {
    fn from(spec: &ConnectionSpec) -> Self {
        Self {
            engine: spec.engine,
            host: spec.host.clone(),
            port: spec.port,
            database: spec.database.clone(),
            user: spec.user.clone(),
            auth: spec.auth,
            options: spec.options.clone(),
            ssh: spec.ssh.clone(),
        }
    }
}
#[derive(Default)]
struct Boundaries {
    authentication: AtomicU32,
    secret_accesses: AtomicU32,
    factories: AtomicU32,
    sources: AtomicU32,
    descriptions: AtomicU32,
    closes: AtomicU32,
    authenticated: AtomicU32,
    hold_authentication: std::sync::atomic::AtomicBool,
    active_authentication: std::sync::atomic::AtomicBool,
    authentication_failure: std::sync::atomic::AtomicBool,
    unconfirmed_shutdown: std::sync::atomic::AtomicBool,
    obstruct_manifest_on_close: Mutex<Option<std::path::PathBuf>>,
    entered: Notify,
    release: Notify,
    requests: Mutex<Vec<SavedRoute>>,
    constructed: Mutex<Vec<SavedRoute>>,
}
struct AuthenticationOwner(Arc<Boundaries>);
impl Drop for AuthenticationOwner {
    fn drop(&mut self) {
        self.0.active_authentication.store(false, Ordering::SeqCst);
    }
}
struct Credentials(Arc<Boundaries>);
#[async_trait::async_trait]
impl DatabaseAuthentication for Credentials {
    fn auth_kind(&self) -> AuthKind {
        AuthKind::Entra
    }
    async fn credential(&self, spec: &ConnectionSpec, _: &dyn culprit_auth::SecretStore) -> Result<DatabaseCredential> {
        self.0.authentication.fetch_add(1, Ordering::SeqCst);
        self.0.requests.lock().unwrap().push(SavedRoute::from(spec));
        self.0.active_authentication.store(true, Ordering::SeqCst);
        let _owner = AuthenticationOwner(self.0.clone());
        self.0.entered.notify_one();
        if self.0.hold_authentication.load(Ordering::SeqCst) {
            self.0.release.notified().await;
        }
        if self.0.authentication_failure.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorCode::PermissionDenied, "fixture authentication rejected"));
        }
        self.0.authenticated.fetch_add(1, Ordering::SeqCst);
        Ok(DatabaseCredential { token: Zeroizing::new("admission-fixture-private-token".into()), username: None })
    }
}
struct UnusedSecrets(Arc<Boundaries>);
impl culprit_auth::SecretStore for UnusedSecrets {
    fn get(&self, _: &str) -> Result<Option<String>> {
        self.touched()
    }
    fn set(&self, _: &str, _: &str) -> Result<()> {
        self.touched()
    }
    fn delete(&self, _: &str) -> Result<()> {
        self.touched()
    }
    fn forget(&self, _: &str) {
        self.0.secret_accesses.fetch_add(1, Ordering::SeqCst);
    }
}
impl UnusedSecrets {
    fn touched<T>(&self) -> Result<T> {
        self.0.secret_accesses.fetch_add(1, Ordering::SeqCst);
        Err(Error::internal("the admission fixture must not touch a secret store"))
    }
}
struct ProviderProbe(Arc<Boundaries>);
#[async_trait::async_trait]
impl Provider for ProviderProbe {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }
    fn telemetry_source(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.0.sources.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(SourceProbe(self.0.clone())))
    }
    async fn probe(&self) -> Result<ServerInfo> {
        Err(unused())
    }
    async fn snapshot(&self, _: &str, _: &SnapshotOptions) -> Result<Snapshot> {
        Err(unused())
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        Err(unused())
    }
    async fn live(&self) -> Result<LiveState> {
        Err(unused())
    }
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(unused())
    }
}
struct SourceProbe(Arc<Boundaries>);
#[async_trait::async_trait]
impl TelemetrySource for SourceProbe {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.0.descriptions.fetch_add(1, Ordering::SeqCst);
        Err(unused())
    }
    fn query_count(&self) -> u32 {
        self.0.descriptions.load(Ordering::SeqCst)
    }
    async fn read_page(&mut self, _: TelemetryStream, _: Option<&str>) -> Result<CollectionRead> {
        Err(unused())
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        Err(unused())
    }
    async fn close(&mut self) -> SourceShutdown {
        self.0.closes.fetch_add(1, Ordering::SeqCst);
        if let Some(captures) = self.0.obstruct_manifest_on_close.lock().unwrap().as_ref() {
            let capture = std::fs::read_dir(captures).unwrap().next().unwrap().unwrap().path();
            let manifest = capture.join("manifest.json");
            std::fs::remove_file(&manifest).unwrap();
            std::fs::create_dir(&manifest).unwrap();
        }
        if self.0.unconfirmed_shutdown.load(Ordering::SeqCst) {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        }
    }
}
fn unused() -> Error {
    Error::new(ErrorCode::Unsupported, "unused admission fixture operation")
}
fn configured(path: &Path, state: &Arc<Boundaries>) -> Workspace {
    let factories = state.clone();
    let workspace =
        Workspace::open_in(path).with_authentication_provider(Arc::new(Credentials(state.clone()))).with_provider(
            Engine::Postgres,
            ProviderRegistration::new(
                Arc::new(move |spec| {
                    factories.factories.fetch_add(1, Ordering::SeqCst);
                    factories.constructed.lock().unwrap().push(SavedRoute::from(spec));
                    assert_eq!(spec.token.as_deref(), Some("admission-fixture-private-token"));
                    Ok(Box::new(ProviderProbe(factories.clone())))
                }),
                vec![],
            )
            .unwrap(),
        );
    workspace
        .add_connection(NewConnection {
            name: "pilot".into(),
            input: "host=fixture.postgres.database.azure.com port=5432 dbname=orders user=reader".into(),
            engine: Some(Engine::Postgres),
            auth: Some(AuthKind::Entra),
            ..Default::default()
        })
        .unwrap();
    workspace.with_secrets(Arc::new(UnusedSecrets(state.clone())))
}
fn baseline_request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget::default(),
    }
}
fn admitted(workspace: &Workspace) -> FleetSourceAdmission {
    FleetSourceAdmission {
        window: CollectionAdmissionWindow::new(Instant::now(), &baseline_request().budget).unwrap(),
        route_revision: Some(workspace.connection_service().route_revision("pilot").unwrap()),
        investigation: None,
    }
}
fn assert_no_source(state: &Boundaries) {
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert_eq!(state.sources.load(Ordering::SeqCst), 0);
    assert_eq!(state.descriptions.load(Ordering::SeqCst), 0);
    assert_eq!(state.secret_accesses.load(Ordering::SeqCst), 0);
}

#[test]
fn native_route_revision_reads_only_saved_metadata_with_hostile_authentication_and_ssh() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Boundaries::default());
    let workspace = configured(directory.path(), &state);
    for auth in [AuthKind::Password, AuthKind::Entra, AuthKind::AwsIam, AuthKind::None] {
        workspace
            .store
            .update("pilot", |saved| {
                saved.auth = auth;
                saved.password_saved = Some(true);
                saved.ssh = Some(SshConfig {
                    destination: "invalid ssh destination with spaces".into(),
                    port: Some(0),
                    identity_file: Some("/not-a-readable-private-key".into()),
                });
                saved.options.insert("sslrootcert".into(), "/no-readable-root-certificate".into());
            })
            .unwrap();
        let before = std::fs::read(workspace.paths.connections_file()).unwrap();
        let revision = workspace.connection_service().route_revision("pilot").unwrap();
        assert!(valid_digest(&revision));
        assert_eq!(workspace.connection_service().route_revision("pilot").unwrap(), revision);
        assert_eq!(std::fs::read(workspace.paths.connections_file()).unwrap(), before);
    }
    assert_eq!(state.authentication.load(Ordering::SeqCst), 0);
    assert_no_source(&state);
    assert!(!workspace.collection_admission_path().exists());
    assert!(!workspace.source_gate.is_closed());
}

#[tokio::test(start_paused = true)]
async fn a_mutated_saved_route_or_expired_start_fails_before_ssh_authentication_or_provider_construction() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Boundaries::default());
    let workspace = configured(directory.path(), &state);
    let original = admitted(&workspace);
    workspace
        .store
        .update("pilot", |saved| {
            saved.host = "a-different-host.invalid".into();
            saved.ssh = Some(SshConfig {
                destination: "invalid ssh destination with spaces".into(),
                port: Some(0),
                identity_file: None,
            });
        })
        .unwrap();
    let error = workspace.connection_service().provider_admitted("pilot", &original).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    let expired = admitted(&workspace);
    tokio::time::advance(culprit_collector::capture::SOURCE_ADMISSION_START_WINDOW).await;
    let error = workspace.connection_service().provider_admitted("pilot", &expired).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert_eq!(state.authentication.load(Ordering::SeqCst), 0);
    assert_no_source(&state);
    assert!(!workspace.collection_admission_path().exists());
}

#[tokio::test(start_paused = true)]
async fn admitted_authentication_and_factory_share_one_frozen_saved_route_during_concurrent_edits() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Boundaries::default());
    state.hold_authentication.store(true, Ordering::SeqCst);
    let workspace = configured(directory.path(), &state);
    let admission = admitted(&workspace);
    let original = SavedRoute::from(&workspace.store.get("pilot").unwrap().spec());
    let connections = workspace.connection_service();
    let provider = connections.provider_admitted("pilot", &admission);
    tokio::pin!(provider);
    tokio::select! { biased; _ = &mut provider => panic!("authentication must still own its gated future"), _ = state.entered.notified() => {} }
    workspace
        .store
        .update("pilot", |saved| {
            saved.host = "replacement.invalid".into();
            saved.port = Some(6543);
            saved.database = Some("replacement".into());
            saved.user = Some("replacement-user".into());
            saved.auth = AuthKind::None;
            saved.environment = Environment::Development;
            saved.options.insert("application_name".into(), "replacement".into());
            saved.ssh = Some(SshConfig {
                destination: "invalid ssh destination with spaces".into(),
                port: Some(0),
                identity_file: None,
            });
        })
        .unwrap();
    state.release.notify_one();
    drop(provider.await.unwrap());
    assert_eq!(*state.requests.lock().unwrap(), vec![original.clone()]);
    assert_eq!(*state.constructed.lock().unwrap(), vec![original]);
    assert_ne!(
        workspace.connection_service().route_revision("pilot").unwrap(),
        admission.route_revision.as_deref().unwrap()
    );
    assert_eq!(state.authentication.load(Ordering::SeqCst), 1);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert_eq!(state.sources.load(Ordering::SeqCst), 0);
    assert_eq!(state.secret_accesses.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn authentication_that_outlives_the_reserved_start_never_reaches_the_lazy_provider_factory() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Boundaries::default());
    state.hold_authentication.store(true, Ordering::SeqCst);
    let workspace = configured(directory.path(), &state);
    let admission = admitted(&workspace);
    let connections = workspace.connection_service();
    let provider = connections.provider_admitted("pilot", &admission);
    tokio::pin!(provider);
    tokio::select! { biased; _ = &mut provider => panic!("authentication must still own its gated future"), _ = state.entered.notified() => {} }
    tokio::time::advance(culprit_collector::capture::SOURCE_ADMISSION_START_WINDOW).await;
    let error = provider.await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert_eq!(state.authentication.load(Ordering::SeqCst), 1);
    assert_eq!(state.authenticated.load(Ordering::SeqCst), 0);
    assert!(!state.active_authentication.load(Ordering::SeqCst));
    state.release.notify_one();
    assert_no_source(&state);
}

#[tokio::test]
async fn capture_controller_cancels_late_authentication_without_telemetry_or_native_debt() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Boundaries::default());
    state.hold_authentication.store(true, Ordering::SeqCst);
    let workspace = configured(directory.path(), &state);
    let mut outbox = Outbox::create(&directory.path().join("outbox"), OutboxLimits::default()).unwrap();
    outbox.recover().await.unwrap();
    let admission = admitted(&workspace);
    let request = baseline_request();
    let cancellation = CollectionCancellation::default();
    let manifest = {
        let capture = workspace.collect_into_outbox_admitted("pilot", request, &mut outbox, &cancellation, &admission);
        tokio::pin!(capture);
        tokio::select! { biased; _ = &mut capture => panic!("capture must reach the controlled authentication boundary"), _ = state.entered.notified() => {} }
        // Filesystem setup has completed. Pause only now, so real blocking I/O cannot advance this test's clock to the start deadline.
        tokio::time::pause();
        tokio::time::advance(culprit_collector::capture::SOURCE_ADMISSION_START_WINDOW + Duration::from_millis(1))
            .await;
        let outcome = capture.await;
        assert_eq!(
            outcome.source_work,
            SourceWorkState::Quiescent,
            "the returning controller proved that no native source remained active"
        );
        assert!(
            outcome.attempt.as_ref().is_some_and(|receipt| receipt.is_valid() && receipt.manifest_sha256.is_some())
        );
        outcome.result.unwrap()
    };
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
    assert_eq!(manifest.source_queries, 0);
    assert_eq!(state.authenticated.load(Ordering::SeqCst), 0);
    assert!(!state.active_authentication.load(Ordering::SeqCst));
    assert_no_source(&state);
    assert!(!serde_json::to_string(&manifest).unwrap().contains("admission-fixture-private-token"));
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(!workspace.source_gate.is_closed());
    let expired = workspace
        .collect_into_outbox_admitted(
            "pilot",
            baseline_request(),
            &mut outbox,
            &CollectionCancellation::default(),
            &admission,
        )
        .await;
    assert_eq!(expired.source_work, SourceWorkState::NotStarted);
    assert!(expired.attempt.is_none(), "an expired dispatch must not invent an allocated attempt");
    assert_eq!(CollectionStop::from_error(&expired.result.unwrap_err()), CollectionStop::TimedOut);
    let status = outbox.status(10, None).await.unwrap();
    assert_eq!((status.captures, status.source_debts, status.unconfirmed_sources), (1, 0, 0));
    assert_eq!(status.entries[0].source_work, SourceWorkState::NotStarted);
}

#[tokio::test]
async fn an_unchanged_admitted_route_can_reach_the_source_and_settle_its_confirmed_close() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Boundaries::default());
    let workspace = configured(directory.path(), &state);
    let mut outbox = Outbox::create(&directory.path().join("outbox"), OutboxLimits::default()).unwrap();
    outbox.recover().await.unwrap();
    let outcome = workspace
        .collect_into_outbox_admitted(
            "pilot",
            baseline_request(),
            &mut outbox,
            &CollectionCancellation::default(),
            &admitted(&workspace),
        )
        .await;
    assert_eq!(outcome.source_work, SourceWorkState::Quiescent);
    let receipt = outcome.attempt.unwrap();
    assert!(receipt.is_valid());
    assert!(receipt.manifest_sha256.is_some(), "describe failed after the initial manifest was durably written");
    assert_eq!(outbox.attempt_receipt(&receipt.capture, None).await.unwrap(), receipt);
    let error = outcome.result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported, "the fixture's native describe boundary must actually run");
    assert_eq!(state.authentication.load(Ordering::SeqCst), 1);
    assert_eq!(state.authenticated.load(Ordering::SeqCst), 1);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert_eq!(state.sources.load(Ordering::SeqCst), 1);
    assert_eq!(state.descriptions.load(Ordering::SeqCst), 1);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(state.secret_accesses.load(Ordering::SeqCst), 0);
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(!workspace.source_gate.is_closed());
}

#[tokio::test]
async fn rejected_authentication_retains_allocated_manifest_before_native_description() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Boundaries::default());
    state.authentication_failure.store(true, Ordering::SeqCst);
    let workspace = configured(directory.path(), &state);
    let path = directory.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let outcome = workspace
        .collect_into_outbox_admitted(
            "pilot",
            baseline_request(),
            &mut outbox,
            &CollectionCancellation::default(),
            &admitted(&workspace),
        )
        .await;
    assert_eq!(outcome.source_work, SourceWorkState::Quiescent);
    assert_eq!(outcome.result.unwrap_err().code, ErrorCode::PermissionDenied);
    let receipt = outcome.attempt.unwrap();
    assert!(receipt.is_valid());
    let bytes =
        std::fs::read(path.join("captures").join(&receipt.capture.observation_id).join("manifest.json")).unwrap();
    assert_eq!(receipt.manifest_sha256, Some(digest(&bytes)));
    let manifest: CollectionManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(manifest.capture, receipt.capture);
    assert!(manifest.description.is_none());
    assert!(matches!(manifest.status, CollectionStatus::Incomplete(_)));
    assert_eq!(manifest.source_queries, 0);
    assert_eq!(state.authentication.load(Ordering::SeqCst), 1);
    assert_no_source(&state);
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn invalid_manifest_receipt_cannot_erase_allocated_identity_or_unconfirmed_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Boundaries::default());
    state.unconfirmed_shutdown.store(true, Ordering::SeqCst);
    let path = directory.path().join("outbox");
    *state.obstruct_manifest_on_close.lock().unwrap() = Some(path.join("captures"));
    let workspace = configured(directory.path(), &state);
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let outcome = workspace
        .collect_into_outbox_admitted(
            "pilot",
            baseline_request(),
            &mut outbox,
            &CollectionCancellation::default(),
            &admitted(&workspace),
        )
        .await;
    assert_eq!(outcome.source_work, SourceWorkState::Unconfirmed);
    let receipt = outcome.attempt.unwrap();
    assert!(receipt.is_valid());
    assert!(receipt.manifest_sha256.is_none(), "an invalid existing artifact cannot acquire an attested digest");
    assert_eq!(outcome.result.unwrap_err().detail, Some(ErrorDetail::Outbox(OutboxProblem::Integrity)));
    let status = outbox.status(10, None).await.unwrap();
    assert_eq!((status.captures, status.source_debts, status.ready), (1, 1, 0));
    assert_eq!(status.entries[0].capture, receipt.capture);
    assert_eq!(status.entries[0].source_work, SourceWorkState::Unconfirmed);
    assert!(workspace.source_gate.is_closed());
    assert!(workspace.collection_admission_status().await.unwrap().source_quiescence_required);
}
