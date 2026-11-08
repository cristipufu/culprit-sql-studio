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
    let format = culprit_core::SNAPSHOT_FORMAT;
    let summary = format!(
        r#"{{"format":{format},"id":"{id}","taken_at":"2026-10-01T00:00:00Z","opportunities":3,"headline":"{name}"}}"#
    );
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

// What a person reads when a connection, a period or a pasted deadlock can't be used, word for word: these messages come
// from locales/en.json.
#[tokio::test]
async fn what_cannot_be_done_with_a_connection_says_why_in_english() {
    let home = temp_home("english");
    let ws = Workspace::open_in(&home);
    let said = |result: Result<ConnectionInfo>| {
        let e = result.unwrap_err();
        (e.message, e.hint)
    };
    assert_eq!(
        said(ws.add_connection(NewConnection { name: "x".into(), input: "db.example".into(), ..Default::default() })),
        (
            "Can't tell which database engine this is".into(),
            Some("Choose the engine explicitly: PostgreSQL, SQL Server, MySQL or SQLite (--engine in the CLI)".into())
        )
    );
    let file = home.join("app.db");
    ws.add_connection(NewConnection {
        name: "file".into(),
        input: file.to_string_lossy().into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(said(ws.set_password("file", "x")).0, "'file' does not use password authentication");
    ws.add_connection(NewConnection {
        name: "shop".into(),
        input: "postgres://app@db.example:5432/shop".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(said(ws.set_password("shop", "")).0, "The password is empty");
    let missing = home.join("missing.log");
    let (message, hint) = said(ws.set_query_log("file", Some(&missing)));
    assert!(message.starts_with(&format!("Can't read {}: ", missing.display())), "{message}");
    assert_eq!(hint.as_deref(), Some("Give the path of the log file the application writes, e.g. log/production.log."));
    let folder = std::fs::canonicalize(&home).unwrap();
    assert_eq!(said(ws.set_query_log("file", Some(&home))).0, format!("{} isn't a file", folder.display()));

    let candidate = ImportCandidate {
        alias: "shop".into(),
        source: ImportSource::Ads,
        origin: "shop".into(),
        file: "settings.json".into(),
        engine: Engine::Postgres,
        environment: Environment::Development,
        host: "db.example".into(),
        port: Some(5432),
        database: Some("shop".into()),
        user: Some("app".into()),
        auth: AuthKind::Password,
        options: Default::default(),
        note: None,
        saved_as: None,
    };
    let e = ws.import_connections(&[candidate]).unwrap_err();
    assert_eq!(
        (e.message.as_str(), e.hint.as_deref()),
        ("A connection named 'shop' already exists", Some("Pick another alias for it"))
    );

    let discovered = |cloud: Cloud| DiscoveredDatabase {
        alias: "yb".into(),
        cloud,
        engine: Engine::Yugabyte,
        environment: Environment::Development,
        server: "yb".into(),
        host: "yb.example".into(),
        database: "yugabyte".into(),
        location: String::new(),
        subscription: String::new(),
        resource_group: String::new(),
        port: None,
        user: None,
        region: None,
        auth: AuthKind::Password,
        saved_as: None,
    };
    assert_eq!(said(ws.add_discovered(&discovered(Cloud::Aws))).0, "YugabyteDB is not an Amazon RDS engine");
    assert_eq!(
        said(ws.add_discovered(&discovered(Cloud::Azure))).0,
        "YugabyteDB is not an Azure flexible-server engine"
    );

    let workspace = &ws;
    let refused = |input: &str| {
        let new = NewConnection {
            name: "iam".into(),
            input: input.into(),
            auth: Some(AuthKind::AwsIam),
            ..Default::default()
        };
        async move { workspace.provider_for(&new).await.err().expect("refused before the AWS CLI") }
    };
    assert_eq!(
        refused("postgres://db.example/shop").await.message,
        "AWS IAM authentication needs a database user name"
    );
    assert_eq!(
        refused("yugabyte://app@db.example/shop").await.message,
        "AWS RDS IAM authentication does not apply to YugabyteDB"
    );
    let e = refused("postgres://app@db.example/shop").await;
    assert_eq!(e.message, "Can't tell which AWS region this RDS endpoint is in");
    assert_eq!(e.hint.as_deref(), Some("Add ?region=… to the connection"));

    let variable = "CULPRIT_ENGLISH_TEST_UNSET_PASSWORD";
    ws.add_connection(NewConnection {
        name: "from-env".into(),
        input: "postgres://app@db.example:5432/shop".into(),
        password_env: Some(variable.into()),
        ..Default::default()
    })
    .unwrap();
    let e = ws.provider("from-env").await.err().expect("no password to read");
    assert_eq!(e.message, format!("Environment variable {variable} is not set"));
    assert_eq!(e.hint, Some(format!("Connection 'from-env' reads its password from {variable}")));

    ws.save_query(saved::SaveQuery {
        name: "q".into(),
        sql: "SELECT 1".into(),
        connection: None,
        description: None,
        previous_name: None,
    })
    .unwrap();
    let e = ws.run_saved("q", None, 10, false).await.unwrap_err();
    assert_eq!(
        (e.message.as_str(), e.hint.as_deref()),
        ("'q' isn't tied to a connection", Some("Pass the connection to run it on."))
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn periods_and_pasted_deadlocks_that_cannot_be_read_say_why_in_english() {
    let e = EventsRequest { start: Some("soon".into()), ..Default::default() }.range().unwrap_err();
    assert_eq!(e.message, "`start` isn't a timestamp: soon");
    assert_eq!(e.hint.as_deref(), Some("Use ISO 8601 with a time zone, e.g. 2026-10-11T14:05:00Z"));
    let backwards = EventsRequest {
        start: Some("2026-10-11T14:05:00Z".into()),
        end: Some("2026-10-10T14:05:00Z".into()),
        ..Default::default()
    };
    assert_eq!(backwards.range().unwrap_err().message, "The period is empty: its start must come before its end");

    let e = deadlocks::parse_deadlock("nothing to see").unwrap_err();
    assert_eq!(e.message, "No deadlock found in that text");
    assert_eq!(
        e.hint.as_deref(),
        Some(
            "Paste a SQL Server deadlock graph (.xdl XML), the LATEST DETECTED DEADLOCK section of SHOW ENGINE INNODB \
             STATUS, or the PostgreSQL log lines from \"deadlock detected\" to STATEMENT."
        )
    );
    assert_eq!(
        deadlocks::parse_deadlock(&"x".repeat(8 * 1024 * 1024 + 1)).unwrap_err().message,
        "That text is larger than 8 MB; paste one deadlock at a time"
    );
}
