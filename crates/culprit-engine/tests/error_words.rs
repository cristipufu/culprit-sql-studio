//! What a person is told when something can't be done, word for word: the errors and hints of reading plans, saved
//! queries, connections, reports, exports, imports, query logs and statements Culprit refuses to run, and the notes an
//! import leaves. Their words come from the language catalog (locales/en.json); the pin in `tests/words/` holds the
//! English to what they said when it was in code. None of these needs a database: each is refused before connecting.

use std::path::{Path, PathBuf};

use culprit_core::{AuthKind, Environment, Error, ExplainMode, SessionAction};
use culprit_engine::export::{ExportFormat, ExportOptions};
use culprit_engine::import::{self, ImportSource};
use culprit_engine::saved::SaveQuery;
use culprit_engine::{NewConnection, Workspace, plans};

use crate::export_words::pin;

#[path = "../../../tests/support/paths.rs"]
mod paths;

struct Home(PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home(tag: &str) -> Home {
    let dir = std::env::temp_dir().join(format!("culprit-error-words-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// An error as a person reads it, with the temporary folder it names left out.
fn said(dir: &Path, what: &str, result: Result<impl Sized, Error>) -> String {
    let error = match result {
        Ok(_) => return format!("{what}: no error"),
        Err(e) => e,
    };
    let text = format!(
        "{what}: [{:?}] {}{}",
        error.code,
        error.message,
        error.hint.map(|h| format!("\n  hint: {h}")).unwrap_or_default()
    );
    paths::normalize(&text, &[(dir, "<dir>")])
}

fn connection(name: &str, input: &str, environment: Environment) -> NewConnection {
    NewConnection {
        name: name.into(),
        input: input.into(),
        environment,
        auth: Some(AuthKind::None),
        ..Default::default()
    }
}

#[test]
fn plans_that_dont_read_say_what_they_said() {
    let dir = Path::new("/nowhere");
    let mut out = Vec::new();
    for (what, input) in [
        ("words", "hello world"),
        ("html", "<html><body/></html>"),
        ("yaml", "- Plan:\n    Node Type: \"Seq Scan\""),
        ("json", "{\"rows\": []}"),
        ("showplan text", "  |--Clustered Index Scan(OBJECT:([db].[dbo].[t].[pk]))"),
        ("empty", ""),
        ("broken showplan", "<ShowPlanXML><unclosed></ShowPlanXML>"),
        ("pg text without nodes", "Planning Time: 0.1 ms\nExecution Time: 0.2 ms"),
        ("culprit json from elsewhere", "{\"root\": {}, \"engine\": \"postgres\", \"total_cost\": 1}"),
    ] {
        out.push(said(dir, what, plans::parse_plan(input)));
    }
    out.push(said(dir, "too large", plans::parse_plan(&"x".repeat(80 * 1024 * 1024))));
    out.push(said(dir, "not text", plans::decode(&[0xC3, 0x28, 0xA0, 0xA1])));
    out.push(said(dir, "bad utf-16", plans::decode(&[0xFF, 0xFE, 0x00, 0xD8, 0x41, 0x00])));
    pin("errors-plans", &out.join("\n"));
}

#[tokio::test]
async fn refusals_say_what_they_said() {
    let home = home("refusals");
    let dir = home.0.as_path();
    let ws = Workspace::open_in(dir);
    ws.add_connection(connection("prod", "postgres://reader@db.invalid/shop", Environment::Production)).unwrap();
    ws.add_connection(connection("dev", "postgres://reader@db.invalid/shop", Environment::Development)).unwrap();
    let mut out = vec![
        // Connections.
        said(dir, "bad name", ws.add_connection(connection("bad name", "postgres://a@b/c", Environment::Development))),
        said(dir, "unknown", ws.connection("nope")),
        said(dir, "rename onto another", ws.rename_connection("dev", "prod")),
        said(dir, "remove unknown", ws.remove_connection("nope")),
        // Saved queries.
        said(dir, "saved unknown", ws.saved_query("nope")),
        said(dir, "delete unknown", ws.delete_saved_query("nope")),
    ];
    let save = |name: &str, sql: &str, previous: Option<&str>| SaveQuery {
        name: name.into(),
        sql: sql.into(),
        connection: None,
        description: None,
        previous_name: previous.map(str::to_string),
    };
    out.push(said(dir, "save no name", ws.save_query(save(" ", "SELECT 1", None))));
    out.push(said(dir, "save no sql", ws.save_query(save("a", " ", None))));
    ws.save_query(save("a", "SELECT 1", None)).unwrap();
    ws.save_query(save("b", "SELECT 2", None)).unwrap();
    out.push(said(dir, "save over another", ws.save_query(save("a", "SELECT 3", Some("b")))));
    // Reports a connection doesn't have yet.
    let options = ExportOptions::default();
    out.push(said(dir, "export finding", ws.export_finding("dev", "x", ExportFormat::Markdown, &options)));
    out.push(said(dir, "export report", ws.export_report("dev", ExportFormat::Html, &options)));
    out.push(said(dir, "verify", ws.verify("dev", &[]).await));
    out.push(said(dir, "dashboard", ws.dashboard("dev", None, false).await));
    out.push(said(dir, "query plan", ws.query_plan("dev", "1", None).await));
    out.push(said(dir, "digest nothing", ws.digest(None, None, None, 5, 5)));
    out.push(said(dir, "digest unscanned", ws.digest(Some("dev"), None, None, 5, 5)));
    out.push(said(dir, "index advice", ws.index_advice("dev")));
    out.push(said(dir, "waits", ws.waits("dev", None)));
    out.push(said(dir, "drift", ws.drift("dev", "prod")));
    out.push(said(dir, "report at", ws.index_advice_at("dev", "20260101T000000.000Z")));
    // Statements refused before connecting.
    out.push(said(dir, "unclosed", ws.run("dev", "SELECT 'open", 10, false).await));
    out.push(said(dir, "two statements", ws.run("dev", "SELECT 1; SELECT 2", 10, false).await));
    out.push(said(dir, "write on production", ws.run("prod", "DELETE FROM orders", 10, false).await));
    out.push(said(
        dir,
        "locking read on production",
        ws.run("prod", "SELECT * FROM orders FOR UPDATE", 10, false).await,
    ));
    out.push(said(dir, "read that writes", ws.run_read("dev", "DELETE FROM orders", 10).await));
    out.push(said(
        dir,
        "analyze a write on production",
        ws.explain("prod", "DELETE FROM orders", ExplainMode::Analyze, true).await,
    ));
    out.push(said(
        dir,
        "with an index that isn't one",
        ws.explain_hypothetical("dev", "SELECT 1", &["DROP TABLE orders".into()]).await,
    ));
    out.push(said(dir, "verify no index", ws.verify_index("dev", "SELECT 1", &[]).await));
    out.push(said(dir, "cancel on production", ws.interrupt_session("prod", "42", SessionAction::Cancel).await));
    out.push(said(dir, "plan with no text", culprit_engine::export_plan(&plan(), "t", ExportFormat::Native, &options)));
    pin("errors-refusals", &out.join("\n"));
}

/// A plan read from a scan: Culprit's model, without the engine's own text.
fn plan() -> culprit_core::Plan {
    let mut plan = plans::parse_plan(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/plans/pg-analyze.txt")).unwrap(),
    )
    .unwrap();
    plan.raw = None;
    plan
}

#[test]
fn files_that_dont_read_say_what_they_said() {
    let home = home("files");
    let dir = home.0.as_path();
    let ws = Workspace::open_in(dir.join("home"));
    let write = |name: &str, text: &str| {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    // A scan saved the way a scan saves it, beside its one-line summary.
    let reports = dir.join("reports");
    let report: culprit_core::Report = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/pg-demo.report.json")).unwrap(),
    )
    .unwrap();
    culprit_engine::reports::save(&reports, &report).unwrap();
    let summary = reports.join("latest.json");
    let other = write("other.json", r#"{"hello": "world"}"#);
    let not_json = write("words.txt", "hello");
    let empty_log = write("app.log", "nothing to see here\n");
    let mut out = vec![
        said(dir, "summary", culprit_engine::reports::load(&summary)),
        said(dir, "not a report", culprit_engine::reports::load(&other)),
        said(dir, "summary as a snapshot", ws.analyze_file(&summary)),
        said(dir, "not a report", ws.analyze_file(&other)),
        said(dir, "not json", ws.analyze_file(&not_json)),
        said(dir, "import unknown tool", ws.import_file(&not_json, None)),
        said(dir, "import bad json", import::read_file(ImportSource::Ads, &not_json)),
        said(dir, "import bad xml", import::read_file(ImportSource::Datagrip, &not_json)),
    ];
    // An application log with no statements in it.
    ws.add_connection(connection("dev", "postgres://reader@db.invalid/shop", Environment::Development)).unwrap();
    let log = said(dir, "log without statements", ws.set_query_log("dev", Some(&empty_log)));
    out.push(log);
    // What an import says of the connections it found.
    let ads = write(
        "settings.json",
        r#"{"datasource.connections": [
            {"options": {"server": "10.0.0.5\\REPORTING", "database": "reports", "authenticationType": "Integrated"},
             "providerName": "MSSQL"},
            {"options": {"server": "sql.internal.example", "database": "shop", "authenticationType": "AzureMFA"},
             "providerName": "MSSQL"}
        ]}"#,
    );
    let found = import::read_file(ImportSource::Ads, &ads).unwrap();
    for c in &found {
        out.push(format!("import note {}: {}", c.alias, c.note.as_deref().unwrap_or("-")));
    }
    let mut hostless = found[0].clone();
    hostless.host = " ".into();
    let mut entra = found[1].clone();
    entra.auth = AuthKind::Entra;
    out.push(said(dir, "import without a host", ws.import_connections(&[hostless])));
    out.push(said(dir, "import entra elsewhere", ws.import_connections(&[entra])));
    pin("errors-files", &out.join("\n"));
}

#[test]
fn engines_that_differ_cant_drift() {
    let read = |name: &str| -> culprit_core::Report {
        serde_json::from_str(
            &std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap(),
        )
        .unwrap()
    };
    let (a, b) = (read("pg-demo.report.json"), read("mysql-demo.report.json"));
    let error = culprit_engine::drift_reports(&a, &b).unwrap_err();
    assert_eq!(
        error.message,
        "'pg-demo' is PostgreSQL and 'mysql-demo' is MySQL; drift compares databases of the same engine"
    );
}
