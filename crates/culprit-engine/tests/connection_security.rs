use async_trait::async_trait;
use culprit_core::{
    AuthKind, ConnectionSecurity, Engine, Environment, Error, ErrorCode, ExplainMode, LiveState, Plan, Provider,
    Result, ResultSet, ServerInfo, Snapshot, SnapshotOptions,
};
use culprit_engine::{
    ConnectionSettings, NewConnection, Workspace,
    secrets::{Memory, SecretStore},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn home() -> PathBuf {
    std::env::temp_dir().join(format!(
        "culprit-security-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

struct Probe {
    engine: Engine,
    started: Option<Arc<tokio::sync::Notify>>,
    released: Arc<AtomicUsize>,
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Provider for Probe {
    fn engine(&self) -> Engine {
        self.engine
    }
    async fn probe(&self) -> Result<ServerInfo> {
        if let Some(started) = &self.started {
            started.notify_one();
            std::future::pending::<()>().await;
        }
        let snapshot: Snapshot = serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        Ok(ServerInfo { engine: self.engine, ..snapshot.server })
    }
    async fn snapshot(&self, _: &str, _: &SnapshotOptions) -> Result<Snapshot> {
        Err(Error::new(ErrorCode::Unsupported, "probe only"))
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        Err(Error::new(ErrorCode::Unsupported, "probe only"))
    }
    async fn live(&self) -> Result<LiveState> {
        Err(Error::new(ErrorCode::Unsupported, "probe only"))
    }
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(Error::new(ErrorCode::Unsupported, "probe only"))
    }
}

#[test]
fn every_provider_can_edit_security_without_replacing_saved_credentials_or_reports() {
    for engine in Engine::ALL {
        let home = home();
        let secrets = Arc::new(Memory::default());
        let ws = Workspace::open_in(&home).with_secrets(secrets.clone());
        let original = ws
            .add_connection(NewConnection {
                name: "saved".into(),
                input: if engine.has_server() {
                    "db.example:3306/orders".into()
                } else {
                    home.join("orders.db").to_string_lossy().into_owned()
                },
                engine: Some(engine),
                user: Some("reader".into()),
                password: engine.has_server().then(|| "test-secret".into()),
                environment: Environment::Production,
                ..Default::default()
            })
            .unwrap();
        let reports = ws.paths().reports_dir("saved");
        std::fs::create_dir_all(&reports).unwrap();
        std::fs::write(reports.join("retained"), "saved history").unwrap();
        let mode = engine
            .spec()
            .connection_security()
            .map(|facts| facts.modes.iter().find(|mode| mode.allows_plaintext).unwrap().value.clone());
        let settings = ConnectionSettings {
            environment: Environment::Staging,
            security: ConnectionSecurity { mode, ..Default::default() },
        };
        let updated = ws.set_connection_settings("saved", &settings).unwrap();
        assert_eq!(updated.environment, Environment::Staging);
        assert_eq!(
            (updated.host, updated.port, updated.database, updated.user, updated.auth, updated.ssh),
            (original.host, original.port, original.database, original.user, original.auth, original.ssh)
        );
        assert_eq!(secrets.get("saved").unwrap().as_deref(), engine.has_server().then_some("test-secret"));
        assert_eq!(std::fs::read_to_string(reports.join("retained")).unwrap(), "saved history");
        assert_eq!(
            ws.connection("saved").unwrap().security.map(|info| info.settings),
            engine.has_server().then_some(settings.security)
        );
        drop(ws);
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test]
async fn draft_tests_use_saved_secrets_and_never_persist_even_when_the_provider_fails() {
    let home = home();
    let ws = Workspace::open_in(&home).with_secrets(Arc::new(Memory::default())).with_provider(
        Engine::Mysql,
        culprit_engine::ProviderRegistration::new(
            Arc::new(|spec| {
                assert_eq!(spec.password.as_deref(), Some("saved-secret"));
                assert_eq!(spec.option("ssl-mode"), Some("DISABLED"));
                Err(Error::new(ErrorCode::ConnectionFailed, "test connection refused"))
            }),
            Vec::new(),
        )
        .unwrap(),
    );
    ws.add_connection(NewConnection {
        name: "imported".into(),
        input: "mariadb://reader@db.example/orders?sslmode=VERIFY_IDENTITY".into(),
        password: Some("saved-secret".into()),
        ..Default::default()
    })
    .unwrap();
    let before = std::fs::read(ws.paths().connections_file()).unwrap();
    let settings = ConnectionSettings {
        environment: Environment::Local,
        security: ConnectionSecurity { mode: Some("DISABLED".into()), ..Default::default() },
    };
    let error = ws.test_connection_settings("imported", &settings).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::ConnectionFailed);
    assert_eq!(std::fs::read(ws.paths().connections_file()).unwrap(), before);
    assert!(ws.connection("imported").unwrap().has_secret);
    drop(ws);
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn new_connections_test_and_save_the_same_explicit_override() {
    let home = home();
    let released = Arc::new(AtomicUsize::new(0));
    let observed = released.clone();
    let ws = Workspace::open_in(&home).with_secrets(Arc::new(Memory::default())).with_provider(
        Engine::Postgres,
        culprit_engine::ProviderRegistration::new(
            Arc::new(move |spec| {
                assert_eq!(spec.option("sslmode"), Some("disable"));
                assert_eq!(spec.password.as_deref(), Some("draft-secret"));
                Ok(Box::new(Probe { engine: Engine::Postgres, started: None, released: observed.clone() }))
            }),
            Vec::new(),
        )
        .unwrap(),
    );
    let new = NewConnection {
        name: "new".into(),
        input: "postgres://reader@db.example/orders?sslmode=verify-full".into(),
        password: Some("draft-secret".into()),
        security: Some(ConnectionSecurity { mode: Some("disable".into()), ..Default::default() }),
        ..Default::default()
    };
    ws.test_connection(&new).await.unwrap();
    assert!(ws.connections().unwrap().is_empty());
    let saved = ws.add_connection(new).unwrap();
    assert_eq!(saved.security.unwrap().settings.mode.as_deref(), Some("disable"));
    ws.probe("new").await.unwrap();
    assert_eq!(released.load(Ordering::SeqCst), 2);
    drop(ws);
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn cancellation_releases_the_draft_provider_without_saving() {
    let home = home();
    let started = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(AtomicUsize::new(0));
    let signal = started.clone();
    let drops = released.clone();
    let ws = Arc::new(
        Workspace::open_in(&home).with_secrets(Arc::new(Memory::default())).with_provider(
            Engine::Postgres,
            culprit_engine::ProviderRegistration::new(
                Arc::new(move |_| {
                    Ok(Box::new(Probe {
                        engine: Engine::Postgres,
                        started: Some(signal.clone()),
                        released: drops.clone(),
                    }))
                }),
                Vec::new(),
            )
            .unwrap(),
        ),
    );
    ws.add_connection(NewConnection {
        name: "saved".into(),
        input: "postgres://reader@db.example/orders".into(),
        auth: Some(AuthKind::None),
        ..Default::default()
    })
    .unwrap();
    let before = std::fs::read(ws.paths().connections_file()).unwrap();
    let task_ws = ws.clone();
    let task = tokio::spawn(async move {
        task_ws
            .test_connection_settings(
                "saved",
                &ConnectionSettings {
                    environment: Environment::Local,
                    security: ConnectionSecurity { mode: Some("disable".into()), ..Default::default() },
                },
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified()).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(released.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(ws.paths().connections_file()).unwrap(), before);
    drop(ws);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn rejected_security_changes_leave_all_saved_metadata_untouched() {
    let home = home();
    let ws = Workspace::open_in(&home);
    ws.add_connection(NewConnection {
        name: "cloud".into(),
        input: "mysql://reader@service.mysql.database.azure.com/orders".into(),
        auth: Some(AuthKind::Entra),
        ..Default::default()
    })
    .unwrap();
    let before = std::fs::read(ws.paths().connections_file()).unwrap();
    assert!(
        ws.set_connection_settings(
            "cloud",
            &ConnectionSettings {
                environment: Environment::Local,
                security: ConnectionSecurity { mode: Some("DISABLED".into()), ..Default::default() }
            }
        )
        .is_err()
    );
    assert_eq!(std::fs::read(ws.paths().connections_file()).unwrap(), before);
    drop(ws);
    std::fs::remove_dir_all(home).unwrap();
}
