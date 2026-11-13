//! What the CLI prints for a person, word for word: its text and Markdown output for commands that read saved files,
//! saved connections and queries, and the errors it reports, run as a person runs them (the built binary, a throwaway
//! workspace, no colour). Its words come from the language catalog (locales/en.json); the pins in `tests/said/` hold
//! the English to what it printed when its words were in code. To write a pin from the code as it is, run its test
//! with `CULPRIT_PIN_RECORD=1`. Nothing here connects to a database.

use super::workspace::home;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "../../../tests/support/paths.rs"]
mod paths;

fn repo() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

fn fixture(name: &str) -> String {
    repo().join("fixtures").join(name).display().to_string()
}

fn plan(name: &str) -> String {
    repo().join("crates/culprit-engine/tests/plans").join(name).display().to_string()
}

/// "scanned 27 days ago" depends on today: what's left is the words around it.
fn without_ages(text: &str) -> String {
    text.lines()
        .map(|line| {
            let Some(at) = line.find("scanned ") else { return line.to_string() };
            let rest = &line[at + "scanned ".len()..];
            let end = rest.find(" ago").map(|e| e + " ago".len()).or_else(|| rest.find("just now").map(|e| e + 8));
            match end {
                Some(end) => format!("{}scanned <age>{}", &line[..at], &rest[end..]),
                None => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs `culprit <args>` in `home` and says what it printed: the command, its standard output, its standard error and
/// its exit code when not 0.
fn run(home: &Path, args: &[&str], stdin: Option<&str>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_culprit"));
    command
        .args(args)
        .env("CULPRIT_HOME", home)
        .env("NO_COLOR", "1")
        .env_remove("CULPRIT_LANG")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("the culprit binary");
    if let Some(text) = stdin {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let mut said = format!("$ culprit {}\n{}", args.join(" "), String::from_utf8_lossy(&output.stdout));
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        said.push_str(&format!("[stderr]\n{stderr}"));
    }
    if !output.status.success() {
        said.push_str(&format!("[exit {}]\n", output.status.code().unwrap_or(-1)));
    }
    let said = paths::normalize(&said, &[(&repo(), "<repo>"), (home, "<home>")]);
    without_ages(&said)
}

/// `text` is what `tests/said/<name>.txt` holds, word for word; with `CULPRIT_PIN_RECORD` set, it is written there.
#[track_caller]
fn pin(name: &str, text: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/said").join(format!("{name}.txt"));
    if std::env::var_os("CULPRIT_PIN_RECORD").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let pinned = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if text == pinned {
        return;
    }
    let (said, was): (Vec<&str>, Vec<&str>) = (text.lines().collect(), pinned.lines().collect());
    let at = said.iter().zip(&was).position(|(a, b)| a != b).unwrap_or(said.len().min(was.len()));
    let command_start = said[..at.min(said.len())].iter().rposition(|line| line.starts_with("$ culprit ")).unwrap_or(0);
    let command_end = said[at.min(said.len())..]
        .iter()
        .position(|line| line.starts_with("$ culprit "))
        .map_or(said.len(), |offset| at + offset);
    panic!(
        "{name}: line {} differs\n  said:   {:?}\n  pinned: {:?}\nActual command output:\n{}",
        at + 1,
        said.get(at).unwrap_or(&"<nothing>"),
        was.get(at).unwrap_or(&"<nothing>"),
        said[command_start..command_end].join("\n")
    );
}

fn session(home: &Path, commands: &[&[&str]]) -> String {
    commands.iter().map(|args| run(home, args, None)).collect::<Vec<_>>().join("\n")
}

#[test]
fn connections_and_saved_queries_say_what_they_said() {
    let h = home("connections");
    let said = session(
        &h.0,
        &[
            &["--format", "text", "conn", "list"],
            &[
                "--format",
                "text",
                "conn",
                "add",
                "shop",
                "--url",
                "postgres://reader@db.invalid/shop",
                "--env",
                "staging",
                "--no-test",
            ],
            &[
                "--format",
                "text",
                "conn",
                "add",
                "files",
                "--url",
                "Server=tcp:db.invalid;Database=files;User Id=reader",
                "--no-test",
            ],
            &["--format", "text", "conn", "list"],
            &["--format", "text", "conn", "set", "shop", "--env", "production"],
            &["--format", "text", "conn", "set", "shop", "--no-ssh"],
            &["--format", "text", "conn", "set", "shop", "--no-query-log"],
            &["--format", "text", "conn", "set", "shop", "--agent", "off"],
            &["--format", "text", "conn", "set", "shop", "--agent", "stats"],
            &["--format", "text", "conn", "set", "shop", "--agent", "query"],
            &["--format", "text", "conn", "rename", "shop", "shop2"],
            &["--format", "text", "conn", "rm", "shop2"],
            &["--format", "text", "conn", "rm", "shop2"],
            &["--format", "text", "saved", "list"],
            &[
                "--format",
                "text",
                "saved",
                "add",
                "open orders",
                "--sql",
                "SELECT * FROM orders WHERE status = 'open'",
                "--description",
                "The open ones",
            ],
            &["--format", "text", "saved", "list"],
            &["--format", "text", "saved", "show", "open orders"],
            &["--format", "text", "saved", "add", "nothing"],
            &["--format", "text", "saved", "rm", "open orders"],
            &["--format", "text", "saved", "show", "open orders"],
            &["--format", "text", "mark", "files", "--list"],
            &["--format", "text", "mark", "files"],
            &["--format", "text", "audit"],
        ],
    );
    pin("connections", &said);
}

#[test]
fn reports_read_from_files_say_what_they_said() {
    let h = home("reports");
    let mut commands: Vec<Vec<String>> = Vec::new();
    let text =
        |args: &[&str]| -> Vec<String> { ["--format", "text"].iter().chain(args).map(|s| s.to_string()).collect() };
    for report in [
        "mssql-demo.report.json",
        "pg-demo.report.json",
        "mysql-demo.report.json",
        "sqlite-demo.report.json",
        "yugabyte-demo.report.json",
    ] {
        let file = fixture(report);
        commands.push(text(&["findings", "x", "--report", &file]));
        commands.push(text(&["top", "x", "--report", &file]));
        commands.push(text(&["indexes", &file]));
        commands.push(text(&["dashboard", &file]));
        commands.push(text(&["waits", "--file", &file]));
    }
    let (mssql, pg) = (fixture("mssql-demo.report.json"), fixture("pg-demo.report.json"));
    commands.push(text(&["top", "x", "--report", &mssql, "--by", "cpu"]));
    commands.push(vec![
        "--format".into(),
        "md".into(),
        "findings".into(),
        "x".into(),
        "--report".into(),
        mssql.clone(),
    ]);
    commands.push(vec!["--format".into(), "md".into(), "analyze".into(), pg.clone()]);
    commands.push(vec!["--format".into(), "md".into(), "dashboard".into(), pg.clone()]);
    commands.push(text(&["drift", &mssql, &fixture("mssql-demo-staging.report.json")]));
    commands.push(text(&["drift", &pg, &fixture("mysql-demo.report.json")]));
    let lint = "CREATE INDEX ix_orders_status ON orders (status); ALTER TABLE orders ADD COLUMN note text NOT NULL DEFAULT '';";
    commands.push(text(&["lint-sql", "x", "--report", &pg, "--sql", lint]));
    commands.push(text(&["lint-sql", "x", "--report", &pg, "--sql", "SELECT 1"]));
    commands.push(vec![
        "--format".into(),
        "md".into(),
        "lint-sql".into(),
        "x".into(),
        "--report".into(),
        pg.clone(),
        "--sql".into(),
        lint.into(),
    ]);
    commands.push(text(&["show", "x", "nope", "--report", &pg]));
    let said: Vec<String> =
        commands.iter().map(|c| run(&h.0, &c.iter().map(String::as_str).collect::<Vec<_>>(), None)).collect();
    pin("reports", &said.join("\n"));
}

#[test]
fn plans_say_what_they_said() {
    let h = home("plans");
    let mut said = Vec::new();
    for name in [
        "mssql-actual.sqlplan",
        "mssql-scan.sqlplan",
        "pg-analyze.txt",
        "pg-seqscan.json",
        "mysql-analyze.txt",
        "mysql-scan.json",
        "sqlite-scan.txt",
        "yugabyte-scan.json",
    ] {
        said.push(run(&h.0, &["--format", "text", "plan", "show", &plan(name)], None));
    }
    said.push(run(&h.0, &["--format", "text", "plan", "diff", &plan("pg-index.json"), &plan("pg-seqscan.json")], None));
    said.push(run(
        &h.0,
        &["--format", "text", "plan", "diff", &plan("mssql-seek.sqlplan"), &plan("mssql-scan.sqlplan")],
        None,
    ));
    said.push(run(&h.0, &["--format", "text", "plan", "show", "nowhere.sqlplan"], None));
    said.push(run(&h.0, &["--format", "text", "plan", "diff", "-", "-"], None));
    said.push(run(
        &h.0,
        &["--format", "text", "plan", "export", &plan("pg-index.json"), &plan("pg-seqscan.json"), "--native"],
        None,
    ));
    pin("plans", &said.join("\n"));
}

const DEADLOCK: &str = "2026-10-11 13:48:28.887 UTC [539] ERROR:  deadlock detected
2026-10-11 13:48:28.887 UTC [539] DETAIL:  Process 539 waits for ShareLock on transaction 838; blocked by process 545.
\tProcess 545 waits for ShareLock on transaction 837; blocked by process 539.
\tProcess 539: UPDATE customers SET id = id WHERE id = 1
\tProcess 545: UPDATE orders SET id = id WHERE id = 1
2026-10-11 13:48:28.887 UTC [539] HINT:  See server log for query details.
2026-10-11 13:48:28.887 UTC [539] CONTEXT:  while updating tuple (0,1) in relation \"customers\"
2026-10-11 13:48:28.887 UTC [539] STATEMENT:  UPDATE customers SET id = id WHERE id = 1";

#[test]
fn everything_else_says_what_it_said() {
    let h = home("rest");
    let pg = fixture("pg-demo.report.json");
    let mut said = vec![
        run(&h.0, &["--format", "text", "checks"], None),
        run(&h.0, &["--format", "text", "sources"], None),
        run(&h.0, &["--format", "text", "deadlocks", "--file", "-"], Some(DEADLOCK)),
        run(&h.0, &["--format", "text", "deadlocks"], None),
        run(&h.0, &["--format", "text", "deadlocks", "x", "--since", "soon"], None),
        run(&h.0, &["--format", "text", "deadlocks", "x", "--since", "3w"], None),
        run(&h.0, &["--format", "text", "deadlocks", "x", "--since", "0h"], None),
        run(&h.0, &["--format", "text", "waits"], None),
        run(&h.0, &["--format", "text", "waits", "--now"], None),
        run(&h.0, &["--format", "text", "findings", "nope"], None),
        run(&h.0, &["--format", "text", "export", "nope"], None),
        run(&h.0, &["--format", "text", "verify", "nope"], None),
        run(&h.0, &["--format", "text", "lint-sql", "x", "--report", &pg], None),
        run(&h.0, &["--format", "text", "lint-sql", "x", "--report", &pg, "--write"], None),
        run(&h.0, &["--format", "text", "lint-sql", "x", "--report", &pg, "--file", "nowhere.sql"], None),
        run(&h.0, &["--format", "text", "explain", "x"], None),
        run(&h.0, &["--format", "md", "waits", "--file", &pg], None),
        run(&h.0, &["--format", "sarif", "top", "x", "--report", &pg], None),
        run(&h.0, &["--format", "text", "--lang", "xx", "plan", "show", "nowhere"], None),
        run(&h.0, &["--format", "text", "changes", "x", "--recent", "soon"], None),
    ];
    let (mssql, staging) = (fixture("mssql-demo.report.json"), fixture("mssql-demo-staging.report.json"));
    said.push(run(&h.0, &["--format", "sarif", "drift", &mssql, &staging], None));
    pin("rest", &said.join("\n"));
}
