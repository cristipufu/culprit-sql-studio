//! Exact native targets cross the Workspace action boundary; all native work here is a bounded scripted provider.
pub(crate) mod support;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use culprit_core::{
    AuthKind, ConnectionSpec, Engine, Environment, Error, ErrorCode, ErrorDetail, ExplainMode, LiveState, Plan,
    Provider, Result, ResultSet, ServerInfo, SessionAction, SessionActionOutcome, SessionActionRequest,
    SessionActionTarget, Snapshot, SnapshotOptions,
    telemetry::{CollectionStop, SourceShutdown},
};
use culprit_engine::{NewConnection, ProviderRegistration, Workspace};
use culprit_read::admission::SourceAdmission;

struct State {
    current: Mutex<SessionActionTarget>,
    received: Mutex<Vec<(SessionActionTarget, SessionAction)>>,
    signals: AtomicUsize,
    factories: AtomicUsize,
    factory_routes: Mutex<Vec<(String, Environment)>>,
    supported: AtomicBool,
    own_actor: AtomicBool,
    fail_before_signal: AtomicBool,
    uncertain_after_signal: AtomicBool,
    uncertain: AtomicBool,
    hold_action: AtomicBool,
    action_entered: tokio::sync::Notify,
}
impl Default for State {
    fn default() -> Self {
        Self {
            current: Mutex::new(support::target(Engine::Postgres, "42")),
            received: Mutex::new(vec![]),
            signals: AtomicUsize::new(0),
            factories: AtomicUsize::new(0),
            factory_routes: Mutex::new(vec![]),
            supported: AtomicBool::new(true),
            own_actor: AtomicBool::new(false),
            fail_before_signal: AtomicBool::new(false),
            uncertain_after_signal: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
            hold_action: AtomicBool::new(false),
            action_entered: tokio::sync::Notify::new(),
        }
    }
}

struct NativeAction(Arc<State>);
#[async_trait]
impl Provider for NativeAction {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }
    fn source_shutdown(&self) -> SourceShutdown {
        if self.0.uncertain.load(Ordering::Acquire) { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent }
    }
    fn session_actions(&self) -> Vec<SessionAction> {
        if self.0.supported.load(Ordering::Acquire) {
            vec![SessionAction::Cancel, SessionAction::Terminate]
        } else {
            vec![]
        }
    }
    async fn live(&self) -> Result<LiveState> {
        let mut live = support::live(Engine::Postgres, "42");
        live.sessions[0].action_target = Some(self.0.current.lock().unwrap().clone());
        live.session_actions = self.session_actions();
        Ok(live)
    }
    async fn interrupt_session(
        &self,
        target: &SessionActionTarget,
        action: SessionAction,
    ) -> Result<SessionActionOutcome> {
        self.0.received.lock().unwrap().push((target.clone(), action));
        self.0.action_entered.notify_one();
        if self.0.hold_action.load(Ordering::Acquire) {
            std::future::pending::<()>().await;
        }
        target.require_current(&self.0.current.lock().unwrap(), action)?;
        if self.0.own_actor.load(Ordering::Acquire) {
            return Err(Error::invalid("fixture refuses its own native actor"));
        }
        if self.0.fail_before_signal.load(Ordering::Acquire) {
            return Err(SessionActionTarget::signal_failed());
        }
        self.0.signals.fetch_add(1, Ordering::SeqCst);
        self.0.uncertain.store(self.0.uncertain_after_signal.load(Ordering::Acquire), Ordering::Release);
        // This is a recorded dispatch after precheck, never an atomic native generation-CAS claim.
        Ok(SessionActionOutcome::SignalSent)
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
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(unused())
    }
}
fn unused() -> Error {
    Error::new(ErrorCode::Unsupported, "unused fixture operation")
}

fn workspace(path: &std::path::Path, state: &Arc<State>) -> Workspace {
    let state = state.clone();
    Workspace::open_in(path).with_provider(
        Engine::Postgres,
        ProviderRegistration::new(
            Arc::new(move |spec: &ConnectionSpec| {
                state.factories.fetch_add(1, Ordering::SeqCst);
                state.factory_routes.lock().unwrap().push((spec.host.clone(), spec.environment));
                Ok(Box::new(NativeAction(state.clone())))
            }),
            vec![],
        )
        .unwrap(),
    )
}
fn add(workspace: &Workspace, environment: Environment, auth: AuthKind) {
    workspace
        .add_connection(NewConnection {
            name: "pilot".into(),
            input: if auth == AuthKind::Entra {
                "postgres://reader@fixture.postgres.database.azure.com:5432/shop"
            } else {
                "postgres://reader@action.fixture.invalid:5432/shop"
            }
            .into(),
            environment,
            auth: Some(auth),
            ..Default::default()
        })
        .unwrap();
}
async fn request(workspace: &Workspace, action: SessionAction) -> SessionActionRequest {
    workspace.live("pilot").await.unwrap().action_request("42", action).unwrap()
}
async fn debt(workspace: &Workspace) -> culprit_core::telemetry::CollectionAdmissionStatus {
    SourceAdmission::inspect(workspace.paths().data.join("collection-admission")).await.unwrap()
}

#[derive(Default)]
struct Credentials {
    calls: AtomicUsize,
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl culprit_auth::DatabaseAuthentication for Credentials {
    fn auth_kind(&self) -> AuthKind {
        AuthKind::Entra
    }
    async fn credential(
        &self,
        _: &ConnectionSpec,
        _: &dyn culprit_auth::SecretStore,
    ) -> Result<culprit_auth::DatabaseCredential> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hold.load(Ordering::Acquire) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(culprit_auth::DatabaseCredential {
            token: zeroize::Zeroizing::new("synthetic-test-credential".into()),
            username: None,
        })
    }
}

#[tokio::test]
async fn a_frozen_live_target_reaches_one_provider_attempt_and_reports_only_signal_sent() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let workspace = workspace(directory.path(), &state);
    add(&workspace, Environment::Development, AuthKind::None);
    let request = request(&workspace, SessionAction::Cancel).await;
    assert_eq!(workspace.interrupt_session(&request).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(*state.received.lock().unwrap(), vec![(request.target, SessionAction::Cancel)]);
    assert_eq!(state.signals.load(Ordering::SeqCst), 1);
    assert_eq!(
        *state.factory_routes.lock().unwrap(),
        vec![("action.fixture.invalid".into(), Environment::Development); 2]
    );
    assert!(!debt(&workspace).await.source_quiescence_required);
}

#[tokio::test]
async fn invalid_or_unbound_requests_do_not_construct_a_provider_or_start_source_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let workspace = workspace(directory.path(), &state);
    add(&workspace, Environment::Development, AuthKind::None);
    let mut invalid = support::unbound_request("pilot", SessionAction::Cancel);
    invalid.target.activity.request = None;
    assert_eq!(workspace.interrupt_session(&invalid).await.unwrap_err().code, ErrorCode::Unsupported);
    let changed =
        workspace.interrupt_session(&support::unbound_request("pilot", SessionAction::Terminate)).await.unwrap_err();
    assert_eq!(changed.detail, Some(ErrorDetail::SessionActionChanged));
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert!(state.received.lock().unwrap().is_empty());
    assert!(!workspace.paths().data.join("collection-admission").exists());
}

#[tokio::test]
async fn production_refuses_before_target_validation_or_any_factory_work() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let workspace = workspace(directory.path(), &state);
    add(&workspace, Environment::Production, AuthKind::None);
    let mut request = support::unbound_request("pilot", SessionAction::Cancel);
    request.route.revision.clear();
    request.target.activity.session.generation.clear();
    let error = workspace.interrupt_session(&request).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(error.message.contains("pilot"));
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert!(!workspace.paths().data.join("collection-admission").exists());
}

#[tokio::test]
async fn a_saved_route_change_refuses_before_credentials_factory_or_new_admission() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let credentials = Arc::new(Credentials::default());
    let workspace = workspace(directory.path(), &state).with_authentication_provider(credentials.clone());
    add(&workspace, Environment::Development, AuthKind::Entra);
    let request = request(&workspace, SessionAction::Cancel).await;
    let factories = state.factories.load(Ordering::SeqCst);
    let authenticated = credentials.calls.load(Ordering::SeqCst);
    let admission = debt(&workspace).await;
    workspace.set_environment("pilot", Environment::Local).unwrap();
    let error = workspace.interrupt_session(&request).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::SessionActionChanged));
    assert_eq!(credentials.calls.load(Ordering::SeqCst), authenticated);
    assert_eq!(state.factories.load(Ordering::SeqCst), factories);
    assert!(state.received.lock().unwrap().is_empty());
    assert_eq!(debt(&workspace).await, admission);
}

#[tokio::test]
async fn a_route_replaced_during_credential_acquisition_cannot_reach_a_provider_factory_or_signal() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let credentials = Arc::new(Credentials::default());
    let workspace = Arc::new(workspace(directory.path(), &state).with_authentication_provider(credentials.clone()));
    add(&workspace, Environment::Development, AuthKind::Entra);
    let request = request(&workspace, SessionAction::Cancel).await;
    let factories = state.factories.load(Ordering::SeqCst);
    credentials.hold.store(true, Ordering::Release);
    let running = workspace.clone();
    let task = tokio::spawn(async move { running.interrupt_session(&request).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), credentials.entered.notified()).await.unwrap();
    workspace.set_environment("pilot", Environment::Local).unwrap();
    credentials.release.notify_one();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::SessionActionChanged));
    assert_eq!(state.factories.load(Ordering::SeqCst), factories);
    assert!(state.received.lock().unwrap().is_empty());
    assert_eq!(state.signals.load(Ordering::SeqCst), 0);
    assert!(!debt(&workspace).await.source_quiescence_required);
}

#[tokio::test]
async fn changed_native_source_session_request_or_statement_cannot_be_replaced_by_a_fresh_target() {
    for field in 0..6 {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let workspace = workspace(directory.path(), &state);
        add(&workspace, Environment::Development, AuthKind::None);
        let request = request(&workspace, SessionAction::Cancel).await;
        {
            let mut current = state.current.lock().unwrap();
            match field {
                0 => current.activity.session.generation = "replacement-session".into(),
                1 => current.activity.request.as_mut().unwrap().generation = "replacement-request".into(),
                2 => current.statement.as_mut().unwrap().id = "replacement-statement".into(),
                3 => current.context.database_writable = Some(false),
                4 => {
                    current.context.partition =
                        culprit_core::telemetry::SourcePartition::InstanceLocal { member: None, epoch: None }
                }
                _ => {
                    current.context.target =
                        culprit_core::telemetry::CollectionTarget::Database(culprit_core::DatabaseIdentity::server(
                            Engine::Postgres,
                            "other",
                            "other.fixture.invalid",
                            5432,
                            Some("other-generation".into()),
                        ))
                }
            }
        }
        let error = workspace.interrupt_session(&request).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::SessionActionChanged), "field {field}");
        assert_eq!(*state.received.lock().unwrap(), vec![(request.target, SessionAction::Cancel)]);
        assert_eq!(state.signals.load(Ordering::SeqCst), 0);
        assert!(!debt(&workspace).await.source_quiescence_required);
    }
}

#[tokio::test]
async fn whole_session_termination_does_not_cancel_the_replacement_request_by_substitution() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let workspace = workspace(directory.path(), &state);
    add(&workspace, Environment::Development, AuthKind::None);
    let request = request(&workspace, SessionAction::Terminate).await;
    state.current.lock().unwrap().activity.request.as_mut().unwrap().generation = "next-native-request".into();
    assert_eq!(workspace.interrupt_session(&request).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(*state.received.lock().unwrap(), vec![(request.target, SessionAction::Terminate)]);
    assert_eq!(state.signals.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsupported_and_self_targets_send_no_signal_and_settle_their_owned_source() {
    for own in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(State::default());
        let workspace = workspace(directory.path(), &state);
        add(&workspace, Environment::Development, AuthKind::None);
        let request = request(&workspace, SessionAction::Cancel).await;
        state.supported.store(own, Ordering::Release);
        state.own_actor.store(own, Ordering::Release);
        let error = workspace.interrupt_session(&request).await.unwrap_err();
        assert_eq!(error.code, if own { ErrorCode::InvalidInput } else { ErrorCode::Unsupported });
        assert_eq!(state.received.lock().unwrap().len(), usize::from(own));
        assert_eq!(state.signals.load(Ordering::SeqCst), 0);
        assert!(!debt(&workspace).await.source_quiescence_required);
    }
}

#[tokio::test]
async fn a_quiet_failed_attempt_can_be_followed_by_a_new_explicit_action_without_automatic_retry() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let workspace = workspace(directory.path(), &state);
    add(&workspace, Environment::Development, AuthKind::None);
    let request = request(&workspace, SessionAction::Cancel).await;
    state.fail_before_signal.store(true, Ordering::Release);
    assert_eq!(workspace.interrupt_session(&request).await.unwrap_err().code, ErrorCode::QueryFailed);
    assert_eq!(state.received.lock().unwrap().len(), 1);
    assert_eq!(state.signals.load(Ordering::SeqCst), 0);
    assert!(!debt(&workspace).await.source_quiescence_required);
    state.fail_before_signal.store(false, Ordering::Release);
    assert_eq!(workspace.interrupt_session(&request).await.unwrap(), SessionActionOutcome::SignalSent);
    assert_eq!(state.received.lock().unwrap().len(), 2);
    assert_eq!(state.signals.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unconfirmed_shutdown_cannot_return_success_retry_or_release_native_debt_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let first = workspace(directory.path(), &state);
    add(&first, Environment::Development, AuthKind::None);
    let request = request(&first, SessionAction::Cancel).await;
    state.uncertain_after_signal.store(true, Ordering::Release);
    let error = first.interrupt_session(&request).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(state.signals.load(Ordering::SeqCst), 1);
    assert!(debt(&first).await.source_quiescence_required);
    let factories = state.factories.load(Ordering::SeqCst);
    assert!(first.interrupt_session(&request).await.is_err());
    drop(first);
    let restarted = workspace(directory.path(), &state);
    assert!(restarted.interrupt_session(&request).await.is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), factories);
    assert_eq!(state.received.lock().unwrap().len(), 1);
    assert!(debt(&restarted).await.source_quiescence_required);
}

#[tokio::test]
async fn dropping_an_action_future_retains_ownership_and_cannot_start_a_replacement_provider() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let workspace = Arc::new(workspace(directory.path(), &state));
    add(&workspace, Environment::Development, AuthKind::None);
    let request = request(&workspace, SessionAction::Cancel).await;
    state.hold_action.store(true, Ordering::Release);
    let active = workspace.clone();
    let pending = request.clone();
    let task = tokio::spawn(async move { active.interrupt_session(&pending).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), state.action_entered.notified()).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(debt(&workspace).await.source_quiescence_required);
    let factories = state.factories.load(Ordering::SeqCst);
    assert!(workspace.interrupt_session(&request).await.is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), factories);
    assert_eq!(state.signals.load(Ordering::SeqCst), 0);
    drop(workspace);
    let restarted = self::workspace(directory.path(), &state);
    assert!(restarted.interrupt_session(&request).await.is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), factories);
    assert!(debt(&restarted).await.source_quiescence_required);
}
