use super::*;
use culprit_core::{AuthKind, ErrorCode};

fn temp_home(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("culprit-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()))
}

#[test]
fn passwords_live_in_the_workspace_secret_store_not_the_keychain_or_the_file() {
    let home = temp_home("secrets");
    let ws = Workspace::open_in(&home);
    ws.add_connection(NewConnection {
        name: "shop".into(),
        input: "postgres://app:s3cret@db.example:5432/shop".into(),
        ..Default::default()
    })
    .unwrap();
    assert!(ws.connection("shop").unwrap().has_secret);
    assert_eq!(ws.secrets.get("shop").unwrap().as_deref(), Some("s3cret"));
    let file = std::fs::read_to_string(home.join("connections.toml")).unwrap();
    assert!(!file.contains("s3cret"));

    ws.rename_connection("shop", "shop-main").unwrap();
    assert_eq!(ws.secrets.get("shop").unwrap(), None);
    assert_eq!(ws.secrets.get("shop-main").unwrap().as_deref(), Some("s3cret"));
    ws.remove_connection("shop-main").unwrap();
    assert_eq!(ws.secrets.get("shop-main").unwrap(), None);
    let _ = std::fs::remove_dir_all(home);
}

/// A keychain that refuses deletes while `refuse` is set, as one does when the person denies its prompt.
#[derive(Default)]
struct Refusing {
    secrets: secrets::Memory,
    refuse: std::sync::atomic::AtomicBool,
}

impl SecretStore for Refusing {
    fn get(&self, account: &str) -> Result<Option<String>> {
        self.secrets.get(account)
    }
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.secrets.set(account, secret)
    }
    fn delete(&self, account: &str) -> Result<()> {
        if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::new(ErrorCode::Internal, "OS keychain: denied"));
        }
        self.secrets.delete(account)
    }
}

#[test]
fn a_refused_password_delete_keeps_the_whole_connection_so_removing_it_again_works() {
    let home = temp_home("refused-delete");
    let keychain = Arc::new(Refusing::default());
    let ws = Workspace::with_paths(Paths { config: home.clone(), data: home.clone() }, keychain.clone());
    let add = |name: &str, input: &str| {
        ws.add_connection(NewConnection { name: name.into(), input: input.into(), ..Default::default() }).unwrap()
    };
    add("shop", "postgres://app:s3cret@db.example:5432/shop");
    keychain.refuse.store(true, std::sync::atomic::Ordering::SeqCst);

    assert!(ws.remove_connection("shop").is_err());
    assert!(ws.connection("shop").unwrap().has_secret);
    assert_eq!(keychain.get("shop").unwrap().as_deref(), Some("s3cret"));

    // Nothing was saved for a connection added without a password: the keychain's answer doesn't hold it up.
    let imported = add("imported", "postgres://app@db.example:5432/shop");
    assert_eq!((imported.auth, imported.has_secret), (AuthKind::Password, false));
    ws.remove_connection("imported").unwrap();
    assert_eq!(ws.connection("imported").unwrap_err().code, ErrorCode::NotFound);

    keychain.refuse.store(false, std::sync::atomic::Ordering::SeqCst);
    ws.remove_connection("shop").unwrap();
    assert_eq!(ws.connection("shop").unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(keychain.get("shop").unwrap(), None);
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn a_connection_saved_without_a_password_never_sends_one_left_under_its_name() {
    let home = temp_home("stale-secret");
    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = sent.clone();
    let ws = Workspace::open_in(&home).with_provider(
        Engine::Postgres,
        Arc::new(move |spec: &ConnectionSpec| {
            seen.lock().unwrap().push(spec.password.clone());
            Ok(Box::new(Sessions(Engine::Postgres, Arc::default())) as Box<dyn Provider>)
        }),
    );
    ws.add_connection(NewConnection {
        name: "shop".into(),
        input: "postgres://app@other-server.example:5432/shop".into(),
        ..Default::default()
    })
    .unwrap();
    // A password a removed connection named "shop" left behind, for another server.
    ws.secrets.set("shop", "old-server-password").unwrap();
    let _ = ws.probe("shop").await;
    ws.set_password("shop", "new-password").unwrap();
    let _ = ws.probe("shop").await;
    assert_eq!(*sent.lock().unwrap(), vec![None, Some("new-password".to_string())]);
    let _ = std::fs::remove_dir_all(home);
}

fn leave_report(ws: &Workspace, name: &str, id: &str) {
    let dir = ws.paths.reports_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    let summary = format!(r#"{{"id":"{id}","taken_at":"2026-10-01T00:00:00Z","opportunities":3,"headline":"{name}"}}"#);
    std::fs::write(dir.join("latest.json"), summary).unwrap();
}

fn last_report_id(ws: &Workspace, name: &str) -> Option<String> {
    ws.connection(name).unwrap().last_report.map(|r| r.id)
}

#[test]
fn a_name_given_to_another_connection_never_shows_the_removed_ones_reports() {
    let home = temp_home("retired-reports");
    let ws = Workspace::open_in(&home);
    let add = |name: &str, host: &str| {
        ws.add_connection(NewConnection {
            name: name.into(),
            input: format!("postgres://app@{host}:5432/shop"),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap()
    };
    let retired = || -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(ws.paths.removed_reports_dir())
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names
    };

    add("prod", "old-server.example");
    leave_report(&ws, "prod", "old-server-scan");
    ws.remove_connection("prod").unwrap();
    // Removing keeps the reports where `culprit report` and `culprit digest` read them.
    assert!(ws.paths.reports_dir("prod").join("latest.json").exists());
    add("prod", "new-server.example");
    assert_eq!(last_report_id(&ws, "prod"), None);
    let moved = retired();
    assert!(moved.len() == 1 && moved[0].starts_with("prod-"), "{moved:?}");
    let kept = ws.paths.removed_reports_dir().join(&moved[0]).join("latest.json");
    assert!(std::fs::read_to_string(kept).unwrap().contains("old-server-scan"));

    // Renaming into a name a removed connection left reports under brings the connection's own reports along.
    add("gone", "old-server.example");
    leave_report(&ws, "gone", "gone-scan");
    ws.remove_connection("gone").unwrap();
    add("staging", "staging.example");
    leave_report(&ws, "staging", "staging-scan");
    ws.rename_connection("staging", "gone").unwrap();
    assert_eq!(last_report_id(&ws, "gone").as_deref(), Some("staging-scan"));
    assert_eq!(retired().len(), 2);

    // A folder a saved connection reads is never moved, even under a name that differs only in case.
    add("Shop", "shop.example");
    leave_report(&ws, "Shop", "shop-scan");
    add("shop", "other.example");
    assert_eq!(last_report_id(&ws, "Shop").as_deref(), Some("shop-scan"));
    assert_eq!(retired().len(), 2);
    let _ = std::fs::remove_dir_all(home);
}

/// Records interrupts; supports cancelling statements only.
struct Sessions(Engine, Arc<std::sync::Mutex<Vec<(String, SessionAction)>>>);

#[async_trait::async_trait]
impl Provider for Sessions {
    fn engine(&self) -> Engine {
        self.0
    }
    async fn probe(&self) -> Result<ServerInfo> {
        Err(Error::new(ErrorCode::Unsupported, "test double"))
    }
    async fn snapshot(&self, _: &str, _: &SnapshotOptions) -> Result<culprit_core::Snapshot> {
        Err(Error::new(ErrorCode::Unsupported, "test double"))
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        Err(Error::new(ErrorCode::Unsupported, "test double"))
    }
    async fn live(&self) -> Result<LiveState> {
        Ok(LiveState {
            notes: Vec::new(),
            taken_at: String::new(),
            sessions: Vec::new(),
            waits: Vec::new(),
            session_actions: Vec::new(),
            session_actions_note: None,
            blocking: None,
        })
    }
    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(Error::new(ErrorCode::Unsupported, "test double"))
    }
    fn session_actions(&self) -> Vec<SessionAction> {
        vec![SessionAction::Cancel]
    }
    async fn interrupt_session(&self, session: &str, action: SessionAction) -> Result<()> {
        self.1.lock().unwrap().push((session.to_string(), action));
        Ok(())
    }
    async fn query_progress(&self, session: &str, _: bool) -> Result<culprit_core::QueryProgress> {
        Ok(culprit_core::QueryProgress::idle(session, self.0, "2026-10-24T00:00:00Z".into()))
    }
}

#[tokio::test]
async fn sessions_are_never_interrupted_on_production() {
    let home = temp_home("sessions");
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let ws = Workspace::open_in(&home).with_provider(
        Engine::Postgres,
        Arc::new(move |_| Ok(Box::new(Sessions(Engine::Postgres, recorded.clone())) as Box<dyn Provider>)),
    );
    for (name, environment) in [("shop-dev", Environment::Development), ("shop-prod", Environment::Production)] {
        ws.add_connection(NewConnection {
            name: name.into(),
            input: "postgres://app@db.example:5432/shop".into(),
            environment,
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    }

    let prod = ws.live("shop-prod").await.unwrap();
    assert!(prod.session_actions.is_empty() && prod.session_actions_note.is_some());
    let e = ws.interrupt_session("shop-prod", "42", SessionAction::Cancel).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);

    assert_eq!(ws.live("shop-dev").await.unwrap().session_actions, vec![SessionAction::Cancel]);
    assert_eq!(
        ws.interrupt_session("shop-dev", "42", SessionAction::Terminate).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    assert_eq!(
        ws.interrupt_session("shop-dev", "4 2", SessionAction::Cancel).await.unwrap_err().code,
        ErrorCode::InvalidInput
    );
    ws.interrupt_session("shop-dev", "42", SessionAction::Cancel).await.unwrap();
    assert_eq!(*calls.lock().unwrap(), vec![("42".to_string(), SessionAction::Cancel)]);
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn native_sessions_keep_complete_identity_for_progress_and_actions() {
    let home = temp_home("native-sessions");
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let ws = Workspace::open_in(&home).with_provider(
        Engine::Yugabyte,
        Arc::new(move |_| Ok(Box::new(Sessions(Engine::Yugabyte, recorded.clone())) as Box<dyn Provider>)),
    );
    for (name, environment) in [("yb-dev", Environment::Development), ("yb-prod", Environment::Production)] {
        ws.add_connection(NewConnection {
            name: name.into(),
            input: "yugabyte://app@db.example:5433/shop".into(),
            environment,
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    }
    let id = "3d2d9e10-555b-4ae2-a683-a8402505e416/42/2026-10-23T21:00:00.123456Z";
    let progress = ws.query_progress("yb-dev", id, false).await.unwrap();
    assert_eq!(progress.session, id);
    assert_eq!(progress.engine, Engine::Yugabyte);
    for malformed in ["42", "node/42/2026-10-23T21:00:00Z"] {
        assert_eq!(ws.query_progress("yb-dev", malformed, false).await.unwrap_err().code, ErrorCode::InvalidInput);
        assert_eq!(
            ws.interrupt_session("yb-dev", malformed, SessionAction::Cancel).await.unwrap_err().code,
            ErrorCode::InvalidInput
        );
    }
    assert_eq!(
        ws.interrupt_session("yb-prod", id, SessionAction::Cancel).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    ws.interrupt_session("yb-dev", id, SessionAction::Cancel).await.unwrap();
    assert_eq!(*calls.lock().unwrap(), vec![(id.to_string(), SessionAction::Cancel)]);
    let _ = std::fs::remove_dir_all(home);
}
