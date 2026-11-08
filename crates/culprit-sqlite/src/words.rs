//! What the SQLite provider says to a person, word for word: opening errors with their hints, why a file's scan locks
//! a capability, its notes, and what a pasted plan can't be read for. These pin the English as the catalog speaks it.

use std::path::PathBuf;

use culprit_core::{ConnectionSpec, Error, MissingCapability, SshConfig};
use rusqlite::{Connection, ffi};

use crate::collect::*;

fn said(error: &Error) -> String {
    match &error.hint {
        Some(hint) => format!("{} | {hint}", error.message),
        None => error.message.clone(),
    }
}

fn reasons(missing: &[MissingCapability]) -> String {
    missing
        .iter()
        .map(|m| format!("{:?}: {} | {}", m.capability, m.reason, m.fix.as_deref().unwrap_or("-")))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn opening_a_file() {
    let refused = |spec: ConnectionSpec| said(&crate::SqliteProvider::new(&spec).err().expect("refused"));
    assert_eq!(refused(ConnectionSpec::default()), "A SQLite connection needs the database file's path");
    let ssh = SshConfig { destination: "bastion".into(), port: None, identity_file: None };
    assert_eq!(
        refused(ConnectionSpec { database: Some("shop.db".into()), ssh: Some(ssh), ..Default::default() }),
        "SQLite opens a local file and cannot use SSH forwarding"
    );
    assert_eq!(
        said(&crate::open(&PathBuf::from("/nonexistent/culprit/shop.sqlite")).unwrap_err()),
        "No SQLite file at /nonexistent/culprit/shop.sqlite | Check the path; Culprit opens existing files only, \
         read-only."
    );
}

#[test]
fn sqlite_errors() {
    let failure = |code: i32, message: &str| {
        crate::map_error(&rusqlite::Error::SqliteFailure(ffi::Error::new(code), Some(message.into())))
    };
    assert_eq!(
        said(&failure(ffi::SQLITE_INTERRUPT, "interrupted")),
        "The SQLite statement was cancelled or reached its execution deadline | Add a more selective filter or \
         simplify the query."
    );
    assert_eq!(
        said(&failure(ffi::SQLITE_BUSY, "database is locked")),
        "database is locked | SQLite reported a busy or locked file. Another connection or a conflicting transaction \
         can cause this; the error does not measure how long it held the lock. Culprit's read waits at most two \
         seconds where SQLite permits waiting."
    );
    assert_eq!(
        said(&failure(ffi::SQLITE_READONLY, "attempt to write a readonly database")),
        "attempt to write a readonly database | Culprit opens SQLite files read-only and never writes to them."
    );
    assert_eq!(said(&failure(ffi::SQLITE_ERROR, "no such table: x")), "no such table: x");
    assert_eq!(said(&crate::task_stopped(&"task 7 panicked")), "The SQLite task stopped: task 7 panicked");
    assert_eq!(said(&crate::execution_unavailable()), "SQLite execution is unavailable");
}

#[test]
fn what_a_file_scan_notes() {
    let conn = Connection::open_in_memory().unwrap();
    let (_, missing) = capabilities(&conn, Err("the file is 5.0 GB".into()));
    assert_eq!(
        reasons(&missing),
        "Statistics: The file has no planner statistics (sqlite_stat1): the planner guesses how many rows each step reads. | ANALYZE;
TableStats: Row counts are unknown: the file is 5.0 GB. | -
QueryLog: SQLite keeps no history of the statements it ran. Culprit can read them from the log Rails, Django, Entity Framework Core or Phoenix already writes: add it to the connection (`culprit conn set <name> --query-log <file>`, or Add query log in the app). | -
IndexUsage: SQLite keeps no record of which indexes queries use. | -
WaitStats: SQLite has no server, so nothing records waits. | -
Sessions: SQLite has no server, so no sessions to list. | -"
    );
    let mut conn = conn;
    let snapshot = snapshot(
        &mut conn,
        std::path::Path::new("example.sqlite"),
        "example",
        culprit_core::Environment::Local,
        std::time::Instant::now(),
    )
    .unwrap();
    let reason = |name: &str| snapshot.source_outcome(name).and_then(|o| o.reason.clone());
    assert_eq!(
        reason("statistics").as_deref(),
        Some("The file has no sqlite_stat1 table; ANALYZE has not recorded planner statistics.")
    );
    assert_eq!(reason("wal_file").as_deref(), Some("The database is not using WAL journal mode."));

    assert_eq!(no_dbstat(), "this SQLite library has no dbstat table to measure them");
    assert_eq!(too_large(5e9, true), "the file is 4.7 GB: Culprit doesn't read that much of it to count");
    assert_eq!(
        too_large(2e8, false),
        "the file is 191 MB with a rollback journal, where reading it whole would make the application's writes wait"
    );
    assert_eq!(
        reading_stopped("the sqlite statement was cancelled or reached its execution deadline"),
        "reading every page stopped (the sqlite statement was cancelled or reached its execution deadline)"
    );
    assert_eq!(
        sizes_unknown(&no_dbstat()),
        "Table and index sizes are unknown: this SQLite library has no dbstat table to measure them."
    );
    assert_eq!(
        columns_unavailable("docs", "no such module: fts9"),
        "Columns of main.docs are unavailable: no such module: fts9. The table's original definition is retained."
    );
    assert_eq!(pragmas_partial(), "Some file pragmas were not available from this SQLite library.");
    assert_eq!(wal_unreadable(), "WAL file size could not be read; no file contents were inspected.");
    assert_eq!(journal_mode_unknown(), "The journal mode was unavailable, so WAL file health could not be assessed.");
    assert_eq!(blob(12), "<12 bytes>");
    assert_eq!(blob(1), "<1 byte>");
    assert_eq!(blob(12345), "<12345 bytes>");
}

#[test]
fn plans() {
    use crate::plan::{explain, parse_text, plan_from_rows};
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    let plan = explain(&conn, "SELECT name FROM t WHERE id = ?").unwrap();
    let reason = "Planned without parameter values: with statistics that hold value distributions, the chosen index \
                  can depend on them.";
    assert_eq!(plan.facts.generic_plan_reason.as_deref(), Some(reason));
    assert_eq!(plan.props.get("Generic Plan").map(String::as_str), Some(reason));

    let text = |t: &str| said(&parse_text(t).unwrap_err());
    assert_eq!(text("QUERY PLAN\nSCAN t"), "Line 2 isn't a plan step: SCAN t");
    assert_eq!(text("QUERY PLAN\n   |--SCAN t"), "Line 2 skips a parent level");
    assert_eq!(text("1|0|0"), "A SQLite plan row must be id|parent|notused|detail");
    assert_eq!(text("QUERY PLAN"), "The plan has no steps");
    let many = std::iter::repeat_n("1|0|0|SCAN t", 10_001).collect::<Vec<_>>().join("\n");
    assert_eq!(text(&many), "The SQLite plan exceeds 10000 steps");

    let rows = |rows: &[(i64, i64, &str)]| {
        let rows: Vec<(i64, i64, String)> = rows.iter().map(|(a, b, c)| (*a, *b, c.to_string())).collect();
        said(&plan_from_rows("", &rows).unwrap_err())
    };
    assert_eq!(rows(&[(0, 0, "SCAN t")]), "SQLite plan step IDs must be unique positive 32-bit integers");
    assert_eq!(rows(&[(1, 5, "SCAN t")]), "The SQLite plan references a missing parent step");
    assert_eq!(rows(&[(1, 2, "SCAN t"), (2, 1, "SCAN u")]), "The SQLite plan contains a cycle between steps");
    let deep: Vec<(i64, i64, &str)> = (1..=40).map(|i| (i, i - 1, "SCAN t")).collect();
    assert_eq!(rows(&deep), "The SQLite plan exceeds 32 nested steps");
    let wide: Vec<(i64, i64, String)> = (1..=10_001).map(|i| (i, 0, "SCAN t".to_string())).collect();
    assert_eq!(said(&plan_from_rows("", &wide).unwrap_err()), "The SQLite plan exceeds 10000 steps");
}
