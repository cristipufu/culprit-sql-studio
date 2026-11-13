//! Frequent tiny statements: a poll that finds nothing, a write repeated row by row, a read inside a loop. MySQL counts
//! the rows a write changed apart from rows returned, and returns none for a write, so a busy INSERT must not read as a
//! poll.

use culprit_checks::analyze;
use culprit_core::Snapshot;

fn snapshot() -> Snapshot {
    let text = std::fs::read_to_string(format!("{}/../../fixtures/mysql-demo.json", env!("CARGO_MANIFEST_DIR")))
        .expect("fixture");
    let mut s: Snapshot = serde_json::from_str(&text).expect("snapshot");
    // Two hours of statement history: enough for a rate. The statements run over all of it, not the fixture's two
    // minutes, which in two hours would be a burst.
    let workload = s.workload.as_mut().expect("workload");
    workload.window_hours = 2.0;
    for q in &mut workload.queries {
        (q.first_seen, q.last_seen) = (None, None);
    }
    s
}

/// The demo's first statement, made a single-row INSERT run 30K times in the window.
fn with_insert(rows_sent: f64, rows_affected: Option<f64>) -> Snapshot {
    let mut s = snapshot();
    let workload = s.workload.as_mut().expect("workload");
    let q = &mut workload.queries[0];
    q.text = "INSERT INTO `shop`.`events` (`kind`, `at`) VALUES (?, NOW())".into();
    q.executions = 30_000.0;
    q.total_time_ms = 30_000.0 * 0.4;
    q.mean_time_ms = 0.4;
    q.rows = Some(rows_sent);
    q.rows_affected = rows_affected;
    workload.total_executions = workload.queries.iter().map(|q| q.executions).sum();
    s
}

/// The chatty finding about the demo's first statement (the one these tests make an INSERT).
fn chatty_title(s: Snapshot) -> Option<String> {
    let id = s.workload.as_ref().expect("workload").queries[0].id.clone();
    analyze(s).findings.into_iter().find(|f| f.id == format!("chatty-query:{id}")).map(|f| f.title)
}

#[test]
fn a_busy_insert_is_a_row_by_row_write_not_a_poll() {
    let title = chatty_title(with_insert(0.0, Some(30_000.0))).expect("a chatty finding");
    assert!(title.contains("one row each time"), "{title}");
    assert!(!title.contains("find nothing"), "{title}");
}

#[test]
fn without_rows_affected_a_write_is_judged_by_what_the_engine_counted() {
    // Engines that count a write's rows as `rows` (SQL Server, PostgreSQL) leave rows_affected empty.
    let title = chatty_title(with_insert(30_000.0, None)).expect("a chatty finding");
    assert!(title.contains("one row each time"), "{title}");
}

#[test]
fn a_count_over_a_short_window_counts_when_it_already_passes_the_daily_bar() {
    // One minute of history: 30K runs are already more than the 20K a day the check asks for, with nothing stretched;
    // 4K runs in a minute would only reach it by extrapolating to a day.
    let busy = |runs: f64| {
        let mut s = with_insert(0.0, Some(runs));
        let workload = s.workload.as_mut().expect("workload");
        workload.window_hours = 1.0 / 60.0;
        workload.queries[0].executions = runs;
        workload.queries[0].total_time_ms = runs * 0.4;
        workload.total_executions = workload.queries.iter().map(|q| q.executions).sum();
        chatty_title(s).is_some()
    };
    assert!(busy(30_000.0));
    assert!(!busy(4_000.0));
}

#[test]
fn a_read_in_a_loop_costs_round_trips_and_is_verified_over_the_window_it_was_counted_in() {
    // 30K single-row reads in two minutes: a pooled call costs a round trip, not a connection, and two minutes of
    // counters say nothing about a day.
    let mut s = snapshot();
    let workload = s.workload.as_mut().expect("workload");
    workload.window_hours = 2.0 / 60.0;
    let q = &mut workload.queries[0];
    q.text = "SELECT `name`, `price` FROM `shop`.`products` WHERE `id` = ?".into();
    (q.executions, q.total_time_ms, q.mean_time_ms) = (30_000.0, 30_000.0 * 0.4, 0.4);
    (q.rows, q.rows_affected) = (Some(30_000.0), None);
    let id = q.id.clone();
    workload.total_executions = workload.queries.iter().map(|q| q.executions).sum();
    let f = analyze(s).findings.into_iter().find(|f| f.id == format!("chatty-query:{id}")).expect("a chatty read");
    assert!(f.title.ends_with("runs 30K times in 2 minutes, one key at a time"), "{}", f.title);
    assert!(f.summary.contains("Its runs are "), "{}", f.summary);
    assert!(f.summary.contains(" of all statement runs, and each run returns about 1 row."), "{}", f.summary);
    assert!(f.summary.contains("a network round trip.") && !f.summary.contains("connection"), "{}", f.summary);
    // performance_schema keeps running totals: the count a later scan reads still holds every run from before.
    let verify = f.verify.join(" ");
    assert_eq!(
        verify,
        "It ran 30K times in 2 minutes. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."
    );
    assert!(!verify.contains("per day"), "{verify}");
}

// The MySQL demo's fresh reading: "runs 4K times in 6 seconds, one row at a time" for runs that return about 1.6 rows
// each, and "8% of every statement the database executes" for a share of the statements' runs.
#[test]
fn a_chatty_title_says_one_key_a_call_and_the_rows_each_run_handles() {
    let mut s = snapshot();
    let workload = s.workload.as_mut().expect("workload");
    let q = &mut workload.queries[0];
    q.text = "SELECT `id`, `total` FROM `shop`.`orders` WHERE `customer_id` = ?".into();
    (q.executions, q.total_time_ms, q.mean_time_ms) = (30_000.0, 30_000.0 * 0.4, 0.4);
    (q.rows, q.rows_affected) = (Some(48_000.0), None);
    let id = q.id.clone();
    workload.total_executions = workload.queries.iter().map(|q| q.executions).sum();
    let f = analyze(s).findings.into_iter().find(|f| f.id == format!("chatty-query:{id}")).expect("a chatty read");
    assert!(f.title.ends_with(", one key at a time"), "{}", f.title);
    // In the tense the statement's history says (it may have stopped in the fixture's window).
    assert!(f.summary.contains(" of all statement runs, and each run ret"), "{}", f.summary);
    assert!(f.summary.contains(" about 1.6 rows."), "{}", f.summary);
    assert!(!f.summary.contains("every statement the database executes"), "{}", f.summary);
    // A write of 1.6 rows a run isn't "one row each time".
    let f = chatty_write(
        snapshot(),
        "c7b5ac9bb4bcecfe",
        "UPDATE `shop` . `sessions` SET `seen_at` = NOW ( ) WHERE `user_id` = ?",
        Some(48_000.0),
    );
    assert!(f.title.ends_with(", about 1.6 rows each time"), "{}", f.title);
    let f = chatty_write(
        snapshot(),
        "c7b5ac9bb4bcecfe",
        "UPDATE `shop` . `sessions` SET `seen_at` = NOW ( ) WHERE `id` = ?",
        Some(30_000.0),
    );
    assert!(f.title.ends_with(", one row each time"), "{}", f.title);
}

/// The chatty finding about `text`, made the statement of the fixture's query `id`: a single-row write run 30K times
/// in the window.
fn chatty_write(mut s: Snapshot, id: &str, text: &str, rows_affected: Option<f64>) -> culprit_core::Finding {
    let workload = s.workload.as_mut().expect("workload");
    let q = workload.queries.iter_mut().find(|q| q.id == id).expect("query");
    q.text = text.into();
    (q.executions, q.total_time_ms, q.mean_time_ms) = (30_000.0, 30_000.0 * 0.13, 0.13);
    (q.rows, q.rows_affected, q.plan) = (Some(30_000.0), rows_affected, None);
    workload.total_executions = workload.queries.iter().map(|q| q.executions).sum();
    let id = format!("chatty-query:{id}");
    analyze(s).findings.into_iter().find(|f| f.id == id).expect("a chatty write")
}

fn mssql() -> Snapshot {
    let text = std::fs::read_to_string(format!("{}/../../fixtures/mssql-demo.json", env!("CARGO_MANIFEST_DIR")))
        .expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

#[test]
fn a_compound_counter_preserves_its_other_work_and_results() {
    // A conversation's next message number: one row locked and updated per message, not a loop writing row by row.
    let f = chatty_write(
        mssql(),
        "7",
        "(@par0 nvarchar(4000))update [app].[conversations] set [last_seq] = [app].[conversations].[last_seq] + 1, [updated_at] = SYSUTCDATETIME() output INSERTED.[last_seq], INSERTED.[status] where ([app].[conversations].[id] = @par0)",
        None,
    );
    assert!(
        f.summary.contains("adds to last_seq in one row of conversations and returns the new value"),
        "{}",
        f.summary
    );
    assert!(!f.summary.contains("row by row"), "{}", f.summary);
    let fix = f.fix.as_ref().unwrap().summary.clone();
    assert!(fix.contains("preserve every assignment, returned value, row filter and transaction guarantee"), "{fix}");
    assert!(!fix.contains("in the INSERT") && !fix.contains("NEXT VALUE FOR"));
    assert!(!fix.contains("multi-row VALUES") && !fix.contains("bulk insert"), "{fix}");
    assert!(!f.caveats.iter().any(|c| c.contains("lookups")), "{:?}", f.caveats);
    assert!(f.verify[0].contains("numbering, metadata updates and returned values remain correct"));
    // MySQL's counter hands its number back through LAST_INSERT_ID: no sequences there.
    let f = chatty_write(
        snapshot(),
        "c7b5ac9bb4bcecfe",
        "UPDATE `shop` . `counters` SET `n` = LAST_INSERT_ID ( `n` + ? ) WHERE `name` = ?",
        Some(30_000.0),
    );
    let fix = f.fix.unwrap().summary;
    assert!(fix.contains("an AUTO_INCREMENT column") && !fix.contains("SEQUENCE"), "{fix}");
}

#[test]
fn a_single_row_update_is_batched_by_its_keys_not_by_a_bulk_insert() {
    let f = chatty_write(
        snapshot(),
        "c7b5ac9bb4bcecfe",
        "UPDATE `shop` . `sessions` SET `seen_at` = NOW ( ) WHERE `id` = ?",
        Some(30_000.0),
    );
    assert!(
        f.summary.contains("independent requests, row-by-row loops, heartbeats or status changes"),
        "{}",
        f.summary
    );
    assert!(f.summary.contains("counters do not show whether calls already share a transaction"));
    assert!(!f.summary.contains("each one is a round trip and a log flush"));
    let fix = f.fix.as_ref().unwrap().summary.clone();
    assert!(fix.contains("an UPDATE over a list of keys") && !fix.contains("multi-row VALUES"), "{fix}");
    assert!(f.caveats.iter().any(|c| c.contains("single-row updates")), "{:?}", f.caveats);
    // An INSERT keeps the multi-row advice, in MySQL's terms.
    let f = chatty_write(
        snapshot(),
        "c7b5ac9bb4bcecfe",
        "INSERT INTO `shop` . `events` ( `kind` ) VALUES ( ? )",
        Some(30_000.0),
    );
    let fix = f.fix.unwrap().summary;
    assert!(fix.contains("a multi-row VALUES, or LOAD DATA") && !fix.contains("table-valued"), "{fix}");
}

// A staging SQL Server reading: "the row stays locked until its transaction commits, so callers that need the next number of the
// same row wait for each other (4% of database time in all)" read as 4% of database time spent waiting on the
// counter's row, where Query Store measured 1 ms of lock waits: the 4% is the statement's own time.
#[test]
fn a_counter_says_its_share_apart_from_the_lock_waits_measured() {
    let counter = "(@par0 nvarchar(4000))update [app].[conversations] set [last_seq] = [app].[conversations].[last_seq] + 1 output INSERTED.[last_seq] where ([app].[conversations].[id] = @par0)";
    let mut s = mssql();
    let q = s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == "7").expect("query");
    q.waits = vec![
        culprit_core::WaitStat { category: "Lock".into(), wait_type: Some("Lock".into()), wait_ms: 1.0, tasks: None },
        culprit_core::WaitStat {
            category: "IO".into(),
            wait_type: Some("Buffer IO".into()),
            wait_ms: 179.0,
            tasks: None,
        },
    ];
    // This synthetic source explicitly qualifies the entire lock class. A native positive by itself is a subtotal.
    q.waits_coverage = Some(culprit_core::QueryWaitCoverage {
        classes: vec![culprit_core::telemetry::WaitClass::Lock],
        complete: true,
    });
    let mut partial = s.clone();
    partial
        .workload
        .as_mut()
        .unwrap()
        .queries
        .iter_mut()
        .find(|q| q.id == "7")
        .unwrap()
        .waits_coverage
        .as_mut()
        .unwrap()
        .complete = false;
    let f = chatty_write(s, "7", counter, None);
    assert!(
        f.summary
            .contains("so callers that need the next number of the same row can wait for each other. In all it takes "),
        "{}",
        f.summary
    );
    assert!(
        f.summary.ends_with(" of database time, and Query Store recorded 1 ms of lock waits out of its 3.9 s."),
        "{}",
        f.summary
    );
    assert!(!f.summary.contains("in all)"), "{}", f.summary);
    let f = chatty_write(partial, "7", counter, None);
    assert!(
        f.summary.ends_with("At least 1 ms of lock waits were observed; wait coverage is incomplete."),
        "{}",
        f.summary
    );
    assert!(!f.summary.contains("lock waits out of its"), "{}", f.summary);
    // An I/O row cannot qualify an absent lock class, even when I/O coverage is complete.
    let mut s = mssql();
    let q = s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == "7").expect("query");
    q.waits = vec![culprit_core::WaitStat { category: "IO".into(), wait_type: None, wait_ms: 179.0, tasks: None }];
    q.waits_coverage =
        Some(culprit_core::QueryWaitCoverage { classes: vec![culprit_core::telemetry::WaitClass::Io], complete: true });
    let unqualified = chatty_write(s.clone(), "7", counter, None);
    assert!(!unqualified.summary.contains("lock waits"), "{}", unqualified.summary);
    // Only declared lock-class completeness establishes the missing lock row as measured zero.
    s.workload
        .as_mut()
        .unwrap()
        .queries
        .iter_mut()
        .find(|q| q.id == "7")
        .unwrap()
        .waits_coverage
        .as_mut()
        .unwrap()
        .classes
        .push(culprit_core::telemetry::WaitClass::Lock);
    let f = chatty_write(s, "7", counter, None);
    assert!(f.summary.ends_with("Query Store recorded 0 ms of lock waits out of its 3.9 s."), "{}", f.summary);
    // No waits kept for the statement: only its share.
    let mut s = mssql();
    s.workload.as_mut().unwrap().queries.iter_mut().for_each(|q| {
        q.waits.clear();
        q.waits_coverage = None;
    });
    let f = chatty_write(s, "7", counter, None);
    assert!(f.summary.ends_with(" of database time.") && !f.summary.contains("lock waits"), "{}", f.summary);
}

fn demo(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

/// What chatty-query says, before the report refines it, of statement `id` of `s` made `text`, run 30K times in the
/// window, returning `rows` rows and changing `changed` in all, with `tweak` applied after.
fn said(
    mut s: Snapshot,
    id: &str,
    text: &str,
    rows: f64,
    changed: Option<f64>,
    tweak: impl FnOnce(&mut culprit_core::QueryStat),
) -> String {
    let workload = s.workload.as_mut().expect("workload");
    let q = workload.queries.iter_mut().find(|q| q.id == id).expect("query");
    q.text = text.into();
    (q.executions, q.total_time_ms, q.mean_time_ms) = (30_000.0, 30_000.0 * 0.13, 0.13);
    (q.rows, q.rows_affected, q.plan) = (Some(rows), changed, None);
    tweak(q);
    workload.total_executions = workload.queries.iter().map(|q| q.executions).sum();
    let found = crate::run("chatty-query", &s);
    crate::words(found.iter().find(|f| f.id == format!("chatty-query:{id}")).expect("a chatty statement"))
}

// What chatty-query says, word for word, of each shape the demos' goldens (a read in a loop) don't reach: a poll that
// finds nothing, a counter (isolated or not, with the lock waits each engine measured), single-row updates, deletes and
// inserts in each engine's terms, a read of a few rows, a statement that stopped or ran in a burst, and runs Culprit's
// scans measured apart from the totals. Its messages come from locales/en.json.
#[test]
fn chatty_statements_say_what_their_round_trips_do_in_english() {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    assert_eq!(
        crate::info("chatty-query"),
        "Chatty queries\n\
        Tiny queries executed so often that the round trips cost more than the work, typically a loop in application code."
    );
    let none = |_: &mut culprit_core::QueryStat| {};
    let mysql = "c7b5ac9bb4bcecfe";
    let mariadb = || {
        let mut s = snapshot();
        s.server.product = "MariaDB 10.11.8".into();
        s.server.version = "10.11.8-MariaDB".into();
        s
    };
    let counter = "UPDATE `shop` . `counters` SET `n` = LAST_INSERT_ID ( `n` + ? ) WHERE `name` = ?";
    let locked = |q: &mut culprit_core::QueryStat| {
        q.spread = Some(culprit_core::Spread { lock_ms: Some(12.0), ..Default::default() });
    };
    assert_eq!(
        [
            said(snapshot(), mysql, "SELECT `id` FROM `shop`.`jobs` WHERE `status` = ? LIMIT ?", 0.0, None, none),
            said(
                snapshot(),
                mysql,
                "UPDATE `shop` . `sessions` SET `seen_at` = NOW ( ) WHERE `id` = ?",
                0.0,
                Some(30_000.0),
                none
            ),
            said(
                snapshot(),
                mysql,
                "UPDATE `shop` . `sessions` SET `seen_at` = NOW ( ) WHERE `user_id` = ?",
                0.0,
                Some(48_000.0),
                none
            ),
            said(snapshot(), mysql, "DELETE FROM `shop` . `sessions` WHERE `id` = ?", 0.0, Some(30_000.0), none),
            said(snapshot(), mysql, "INSERT INTO `shop` . `events` ( `kind` ) VALUES ( ? )", 0.0, Some(30_000.0), none),
            said(snapshot(), mysql, counter, 0.0, Some(30_000.0), locked),
            said(mariadb(), mysql, counter, 0.0, Some(30_000.0), none),
            said(
                snapshot(),
                mysql,
                "SELECT `id`, `total` FROM `shop`.`orders` WHERE `customer_id` = ?",
                48_000.0,
                None,
                none
            ),
        ],
        [
            "A read from jobs runs 30K times in 2 hours and most runs find nothing · Medium · Medium\n\
                Its runs are 44% of all statement runs, and on average a run returns 0 rows. Empty results can come from polling, request-driven checks or legitimate misses; the counters do not identify the caller or prove a timer. Each run is cheap (9% of database time in all), but unnecessary calls still add round trips.\n\
                impact: 44% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Check the caller first. If it polls for work, back off after empty results, coordinate pollers, or have the writer wake them (a queue or notification). For request-driven checks, reuse an answer only while it remains valid. Keep necessary checks and their correctness guarantees.\n\
                verify: It ran 30K times in 2 hours. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there unnecessary calls and total database time should fall while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent checks are legitimate (a lease or a lock checked on every request). Check the calling code before changing how often it asks.",
            "An update of shop runs 30K times in 2 hours, one row each time · Medium · Medium\n\
                Its runs are 44% of all statement runs, and each run changes about 1 row. Frequent small updates can be independent requests, row-by-row loops, heartbeats or status changes; each call can add a round trip, and each committing transaction can flush the log (9% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 44% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Where the application changes many rows the same way, update them in one statement (an UPDATE over a list of keys (`IN (…)`), or joined to a temporary table of the changes) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 2 hours. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent single-row updates are legitimate (a status that changes as its work progresses). Check the calling code before batching.",
            "An update of shop runs 30K times in 2 hours, about 1.6 rows each time · Medium · Medium\n\
                Its runs are 44% of all statement runs, and each run changes about 1.6 rows. Frequent small updates can be independent requests, row-by-row loops, heartbeats or status changes; each call can add a round trip, and each committing transaction can flush the log (9% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 44% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Where the application changes many rows the same way, update them in one statement (an UPDATE over a list of keys (`IN (…)`), or joined to a temporary table of the changes) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 2 hours. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent single-row updates are legitimate (a status that changes as its work progresses). Check the calling code before batching.",
            "A delete from shop runs 30K times in 2 hours, one row each time · Medium · Medium\n\
                Its runs are 44% of all statement runs, and each run changes about 1 row. Frequent small deletes can be independent removals or a row-by-row loop; each call can add a round trip, and each committing transaction can flush the log (9% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 44% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller already has independent removals that can safely be grouped, delete many rows per statement (an DELETE over a list of keys (`IN (…)`), or joined to a temporary table of the changes, or by a condition the rows share) inside one transaction.\n\
                verify: It ran 30K times in 2 hours. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent single-row deletes are legitimate (a row removed as its work completes). Check the calling code before batching.",
            "An insert into shop runs 30K times in 2 hours, one row each time · Medium · Medium\n\
                Its runs are 44% of all statement runs, and each run changes about 1 row. Frequent small writes can be individual events, heartbeats or a row-by-row loop; each call can add a round trip, and each committing transaction can flush the log (9% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 44% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller already has independent rows that can safely be grouped, write many rows per statement (a multi-row VALUES, or LOAD DATA) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 2 hours. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent single-row writes are legitimate (each event recorded as it happens). Check the calling code before batching.",
            "An update of shop runs 30K times in 2 hours, one row each time · Medium · Medium\n\
                Its runs are 44% of all statement runs. Each run adds to n in one row of counters and returns the new value. It may allocate numbers or maintain other application state; the SQL alone does not establish its business purpose or a removable call. Each call can add a round trip, and the row stays locked until its transaction commits, so callers that need the next number of the same row can wait for each other. In all it takes 9% of database time. At least 12 ms of lock waits were observed; wait coverage is incomplete.\n\
                impact: 44% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller confirms that this is solely a number allocator, consider an AUTO_INCREMENT column only if a database-wide allocator, gaps and allocation order satisfy the application's requirements. It does not preserve gapless or per-parent numbering or commit order. Otherwise keep the atomic UPDATE. Check whether the caller can combine work without losing filters, returned values or transaction guarantees; the SQL alone does not establish a removable call.\n\
                verify: It ran 30K times in 2 hours. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare total time and any avoidable calls, while checking that numbering, metadata updates and returned values remain correct. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: A sequence can have gaps, allocates outside transaction commit order and does not provide per-parent numbering. Check the application's numbering and atomicity requirements before considering one.",
            "An update of shop runs 30K times in 2 hours, one row each time · Medium · Medium\n\
                Its runs are 44% of all statement runs. Each run adds to n in one row of counters and returns the new value. It may allocate numbers or maintain other application state; the SQL alone does not establish its business purpose or a removable call. Each call can add a round trip, and the row stays locked until its transaction commits, so callers that need the next number of the same row can wait for each other. In all it takes 9% of database time.\n\
                impact: 44% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller confirms that this is solely a number allocator, consider a SEQUENCE (`NEXT VALUE FOR`) or an AUTO_INCREMENT column only if a database-wide allocator, gaps and allocation order satisfy the application's requirements. It does not preserve gapless or per-parent numbering or commit order. Otherwise keep the atomic UPDATE. Check whether the caller can combine work without losing filters, returned values or transaction guarantees; the SQL alone does not establish a removable call.\n\
                verify: It ran 30K times in 2 hours. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare total time and any avoidable calls, while checking that numbering, metadata updates and returned values remain correct. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: A sequence can have gaps, allocates outside transaction commit order and does not provide per-parent numbering. Check the application's numbering and atomicity requirements before considering one.",
            "A read from orders runs 30K times in 2 hours, one key at a time · Medium · Medium\n\
                Its runs are 44% of all statement runs, and each run returns about 1.6 rows. Repeated lookups can be per-request checks or a loop in application code (N+1). Each run is cheap (9% of database time in all), but each call can add a network round trip.\n\
                impact: 44% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Reuse a result the application already has, or cache it for as long as it remains valid; access checks must still reflect permission changes. If the caller loops over items, load their rows in one query (WHERE key IN (…) or a join) before the loop; ORMs usually have an eager-loading option for this.\n\
                verify: It ran 30K times in 2 hours. performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some high-frequency single-row lookups are legitimate (for example, by primary key on a hot path). Check the calling code before batching.",
        ]
    );
    let compound = "(@par0 nvarchar(4000))update [app].[conversations] set [last_seq] = [app].[conversations].[last_seq] + 1, [updated_at] = SYSUTCDATETIME() output INSERTED.[last_seq], INSERTED.[status] where ([app].[conversations].[id] = @par0)";
    let isolated = "(@par0 nvarchar(4000))update [app].[conversations] set [last_seq] = [app].[conversations].[last_seq] + 1 output INSERTED.[last_seq] where ([app].[conversations].[id] = @par0)";
    let lock_waits = |q: &mut culprit_core::QueryStat| {
        q.waits = vec![culprit_core::WaitStat { category: "Lock".into(), wait_type: None, wait_ms: 40.0, tasks: None }];
    };
    let end = epoch_seconds("2026-10-11T12:13:47Z").unwrap();
    let stopped = |q: &mut culprit_core::QueryStat| {
        (q.first_seen, q.last_seen) =
            (Some(iso_from_epoch(end - 72.0 * 3600.0)), Some(iso_from_epoch(end - 48.0 * 3600.0)));
    };
    let mut two_days = mssql();
    crate::last_hours(two_days.workload.as_mut().unwrap(), 48.0);
    let burst = |q: &mut culprit_core::QueryStat| {
        (q.first_seen, q.last_seen) = (Some(iso_from_epoch(end - 900.0)), Some(iso_from_epoch(end - 300.0)));
    };
    let read = "SELECT [name] FROM [shop].[customers] WHERE [id] = @P1";
    assert_eq!(
        [
            said(mssql(), "7", compound, 30_000.0, None, none),
            said(mssql(), "7", isolated, 30_000.0, None, lock_waits),
            said(
                mssql(),
                "7",
                "UPDATE [shop].[sessions] SET [seen_at] = SYSUTCDATETIME() WHERE [id] = @P1",
                30_000.0,
                None,
                none
            ),
            said(mssql(), "7", "DELETE FROM [shop].[sessions] WHERE [id] = @P1", 30_000.0, None, none),
            said(mssql(), "7", "INSERT INTO [shop].[events] ([kind]) VALUES (@P1)", 30_000.0, None, none),
            said(mssql(), "7", read, 30_000.0, None, stopped),
            said(mssql(), "7", "SELECT [id] FROM [shop].[jobs] WHERE [status] = @P1", 0.0, None, stopped),
            said(two_days, "7", read, 30_000.0, None, burst),
        ],
        [
            "A query runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 30% of all statement runs. Each run adds to last_seq in one row of conversations and returns the new value. It may allocate numbers or maintain other application state; the SQL alone does not establish its business purpose or a removable call. Each call can add a round trip, and the row stays locked until its transaction commits, so callers that need the next number of the same row can wait for each other. In all it takes 10% of database time.\n\
                impact: 30% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: The SQL does not establish an isolated counter-only operation: preserve every assignment, returned value, row filter and transaction guarantee. Check the caller before changing it. If it already has independent work that can safely share a call, consider batching that work; per-parent numbering and metadata updates may require this atomic UPDATE. A sequence is not a replacement for the whole statement.\n\
                verify: It ran 30K times in 5 hours; after the change compare total time and any avoidable calls, while checking that numbering, metadata updates and returned values remain correct.\n\
                caveat: A sequence can have gaps, allocates outside transaction commit order and does not provide per-parent numbering. Check the application's numbering and atomicity requirements before considering one.",
            "A query runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 30% of all statement runs. Each run adds to last_seq in one row of conversations and returns the new value. It may allocate numbers or maintain other application state; the SQL alone does not establish its business purpose or a removable call. Each call can add a round trip, and the row stays locked until its transaction commits, so callers that need the next number of the same row can wait for each other. In all it takes 10% of database time. At least 40 ms of lock waits were observed; wait coverage is incomplete.\n\
                impact: 30% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller confirms that this is solely a number allocator, consider a SEQUENCE (`NEXT VALUE FOR`) only if a database-wide allocator, gaps and allocation order satisfy the application's requirements. It does not preserve gapless or per-parent numbering or commit order. Otherwise keep the atomic UPDATE. Check whether the caller can combine work without losing filters, returned values or transaction guarantees; the SQL alone does not establish a removable call.\n\
                verify: It ran 30K times in 5 hours; after the change compare total time and any avoidable calls, while checking that numbering, metadata updates and returned values remain correct.\n\
                caveat: A sequence can have gaps, allocates outside transaction commit order and does not provide per-parent numbering. Check the application's numbering and atomicity requirements before considering one.",
            "An update of sessions runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 30% of all statement runs, and each run changes about 1 row. Frequent small updates can be independent requests, row-by-row loops, heartbeats or status changes; each call can add a round trip, and each committing transaction can flush the log (10% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 30% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Where the application changes many rows the same way, update them in one statement (an UPDATE over a list of keys, or joined to a table-valued parameter) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 5 hours; after the change it should run fewer times and take less time in all while the useful work stays the same.\n\
                caveat: Some frequent single-row updates are legitimate (a status that changes as its work progresses). Check the calling code before batching.",
            "A delete from sessions runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 30% of all statement runs, and each run changes about 1 row. Frequent small deletes can be independent removals or a row-by-row loop; each call can add a round trip, and each committing transaction can flush the log (10% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 30% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller already has independent removals that can safely be grouped, delete many rows per statement (an DELETE over a list of keys, or joined to a table-valued parameter, or by a condition the rows share) inside one transaction.\n\
                verify: It ran 30K times in 5 hours; after the change it should run fewer times and take less time in all while the useful work stays the same.\n\
                caveat: Some frequent single-row deletes are legitimate (a row removed as its work completes). Check the calling code before batching.",
            "An insert into events runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 30% of all statement runs, and each run changes about 1 row. Frequent small writes can be individual events, heartbeats or a row-by-row loop; each call can add a round trip, and each committing transaction can flush the log (10% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 30% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller already has independent rows that can safely be grouped, write many rows per statement (a multi-row VALUES, a table-valued parameter, or a bulk insert) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 5 hours; after the change it should run fewer times and take less time in all while the useful work stays the same.\n\
                caveat: Some frequent single-row writes are legitimate (each event recorded as it happens). Check the calling code before batching.",
            "A read from customers ran 30K times in 5 hours, one key at a time · Medium · Medium\n\
                Its runs were 30% of all statement runs, and each run returned about 1 row. Repeated lookups can be per-request checks or a loop in application code (N+1). Each run was cheap (10% of database time in all), but each call can add a network round trip.\n\
                impact: 30% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Reuse a result the application already has, or cache it for as long as it remains valid; access checks must still reflect permission changes. If the caller loops over items, load their rows in one query (WHERE key IN (…) or a join) before the loop; ORMs usually have an eager-loading option for this.\n\
                verify: It ran 30K times in 5 hours; after the change it should run fewer times and take less time in all.\n\
                caveat: Some high-frequency single-row lookups are legitimate (for example, by primary key on a hot path). Check the calling code before batching.",
            "A read from jobs ran 30K times in 5 hours and most runs found nothing · Medium · Medium\n\
                Its runs were 30% of all statement runs, and on average a run returned 0 rows. Empty results can come from polling, request-driven checks or legitimate misses; the counters do not identify the caller or prove a timer. Each run was cheap (10% of database time in all), but unnecessary calls still add round trips.\n\
                impact: 30% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Check the caller first. If it polls for work, back off after empty results, coordinate pollers, or have the writer wake them (a queue or notification). For request-driven checks, reuse an answer only while it remains valid. Keep necessary checks and their correctness guarantees.\n\
                verify: It ran 30K times in 5 hours; after the change unnecessary calls and total database time should fall while the useful work stays the same.\n\
                caveat: Some frequent checks are legitimate (a lease or a lock checked on every request). Check the calling code before changing how often it asks.",
            "A read from customers ran 30K times on Oct 11, 11:58–12:08 UTC, one key at a time · Low · Medium\n\
                Its runs were 30% of all statement runs, and each run returned about 1 row. Repeated lookups can be per-request checks or a loop in application code (N+1). Each run was cheap (10% of database time in all), but each call can add a network round trip.\n\
                impact: 30% of executions\n\
                metric: Runs\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Reuse a result the application already has, or cache it for as long as it remains valid; access checks must still reflect permission changes. If the caller loops over items, load their rows in one query (WHERE key IN (…) or a join) before the loop; ORMs usually have an eager-loading option for this.\n\
                verify: It ran 30K times on Oct 11, 11:58–12:08 UTC; after the change it should run fewer times and take less time in all.\n\
                caveat: Some high-frequency single-row lookups are legitimate (for example, by primary key on a hot path). Check the calling code before batching.",
        ]
    );
    let pg = "2885972271926804069";
    // The PostgreSQL demo's customer lookup over 40 days of running totals, 10K runs a day, where Culprit's three scans
    // of the last 36 hours measured 40K a day; the demo's other statements run `others` times.
    let scanned_as = |others: f64, text: Option<&str>, rows: f64| {
        let mut scanned = demo("postgres-demo.json");
        let w = scanned.workload.as_mut().unwrap();
        let end = epoch_seconds(w.window_end.as_deref().unwrap()).unwrap();
        let at = |hours: f64| iso_from_epoch(end - hours * 3600.0);
        (w.window_start, w.window_hours) = (Some(at(960.0)), 960.0);
        w.timeline = Some(culprit_core::Timeline {
            source: culprit_core::TimelineSource::Scans,
            starts: vec![at(36.0), at(24.0), at(12.0)],
            end: iso_from_epoch(end),
            time_ms: vec![10_000.0; 3],
            executions: vec![40_000.0; 3],
            unknown: None,
        });
        if others > 0.0 {
            w.queries.iter_mut().find(|q| q.id == pg).unwrap().executions = others;
        }
        let q = w.queries.iter_mut().find(|q| q.id == "3432126458605143571").unwrap();
        (q.executions, q.total_time_ms, q.mean_time_ms, q.rows) = (400_000.0, 8_000.0, 0.02, Some(rows));
        if let Some(text) = text {
            q.text = text.into();
        }
        q.first_seen = Some(at(960.0));
        q.trend = Some(culprit_core::QueryTrend { time_ms: vec![Some(400.0); 3], executions: vec![Some(20_000.0); 3] });
        w.total_executions = w.queries.iter().map(|q| q.executions).sum();
        let found = crate::run("chatty-query", &scanned);
        found.iter().find(|f| f.id == "chatty-query:3432126458605143571").map(crate::words)
    };
    let scanned = |others: f64| scanned_as(others, None, 400_000.0);
    let scanned_alone = scanned(0.0);
    assert_eq!(
        [
            said(
                demo("postgres-demo.json"),
                pg,
                "UPDATE sessions SET seen_at = now() WHERE id = $1",
                30_000.0,
                None,
                none
            ),
            said(demo("postgres-demo.json"), pg, "DELETE FROM sessions WHERE id = $1", 30_000.0, None, none),
            said(demo("postgres-demo.json"), pg, "INSERT INTO events (kind) VALUES ($1)", 30_000.0, None, none),
            said(
                demo("postgres-demo.json"),
                pg,
                "UPDATE counters SET n = n + 1 WHERE name = $1 RETURNING n",
                30_000.0,
                None,
                none
            ),
            scanned_alone.expect("a chatty lookup Culprit's scans measured"),
        ],
        [
            "An update of sessions runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 75% of all statement runs, and each run changes about 1 row. Frequent small updates can be independent requests, row-by-row loops, heartbeats or status changes; each call can add a round trip, and each committing transaction can flush the log (2% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 75% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Where the application changes many rows the same way, update them in one statement (an UPDATE over an array of keys (`= ANY($1)`), or joined to a VALUES list) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 5 hours. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent single-row updates are legitimate (a status that changes as its work progresses). Check the calling code before batching.",
            "A delete from sessions runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 75% of all statement runs, and each run changes about 1 row. Frequent small deletes can be independent removals or a row-by-row loop; each call can add a round trip, and each committing transaction can flush the log (2% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 75% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller already has independent removals that can safely be grouped, delete many rows per statement (an DELETE over an array of keys (`= ANY($1)`), or joined to a VALUES list, or by a condition the rows share) inside one transaction.\n\
                verify: It ran 30K times in 5 hours. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent single-row deletes are legitimate (a row removed as its work completes). Check the calling code before batching.",
            "An insert into events runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 75% of all statement runs, and each run changes about 1 row. Frequent small writes can be individual events, heartbeats or a row-by-row loop; each call can add a round trip, and each committing transaction can flush the log (2% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 75% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller already has independent rows that can safely be grouped, write many rows per statement (a multi-row VALUES, arrays unnested into rows, or COPY) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 5 hours. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent single-row writes are legitimate (each event recorded as it happens). Check the calling code before batching.",
            "An update of counters runs 30K times in 5 hours, one row each time · Medium · Medium\n\
                Its runs are 75% of all statement runs. Each run adds to n in one row of counters and returns the new value. It may allocate numbers or maintain other application state; the SQL alone does not establish its business purpose or a removable call. Each call can add a round trip, and the row stays locked until its transaction commits, so callers that need the next number of the same row can wait for each other. In all it takes 2% of database time.\n\
                impact: 75% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller confirms that this is solely a number allocator, consider a sequence (`nextval()`, or an identity column) only if a database-wide allocator, gaps and allocation order satisfy the application's requirements. It does not preserve gapless or per-parent numbering or commit order. Otherwise keep the atomic UPDATE. Check whether the caller can combine work without losing filters, returned values or transaction guarantees; the SQL alone does not establish a removable call.\n\
                verify: It ran 30K times in 5 hours. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare total time and any avoidable calls, while checking that numbering, metadata updates and returned values remain correct. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: A sequence can have gaps, allocates outside transaction commit order and does not provide per-parent numbering. Check the application's numbering and atomicity requirements before considering one.",
            "A read from customers runs about 40K times a day over 36 hours of Culprit's scans, one key at a time · Low · Medium\n\
                Its runs are 50% of all statement runs over 36 hours of Culprit's scans, and each run returns about 1 row (counted over the 40 days since statistics were reset). Repeated lookups can be per-request checks or a loop in application code (N+1). Each run is cheap (4% of database time in all), but each call can add a network round trip.\n\
                impact: 50% of executions\n\
                metric: Runs per day over Culprit's scans\n\
                metric: Runs per day over the 40 days since statistics were reset\n\
                metric: Share of executions over Culprit's scans\n\
                metric: Average duration over Culprit's scans\n\
                fix: Reuse a result the application already has, or cache it for as long as it remains valid; access checks must still reflect permission changes. If the caller loops over items, load their rows in one query (WHERE key IN (…) or a join) before the loop; ORMs usually have an eager-loading option for this.\n\
                verify: It ran about 40K times a day over 36 hours of Culprit's scans. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some high-frequency single-row lookups are legitimate (for example, by primary key on a hot path). Check the calling code before batching.",
        ]
    );
    let sqlite = "c1b0de997fac4faa";
    assert_eq!(
        [
            said(
                demo("sqlite-demo.json"),
                sqlite,
                "UPDATE \"sessions\" SET \"seen_at\" = ? WHERE \"id\" = ?",
                30_000.0,
                None,
                none
            ),
            said(demo("sqlite-demo.json"), sqlite, "DELETE FROM \"sessions\" WHERE \"id\" = ?", 30_000.0, None, none),
            said(
                demo("sqlite-demo.json"),
                sqlite,
                "INSERT INTO \"events\" (\"kind\") VALUES (?)",
                30_000.0,
                None,
                none
            ),
            said(
                demo("sqlite-demo.json"),
                sqlite,
                "UPDATE \"counters\" SET \"n\" = \"n\" + 1 WHERE \"name\" = ? RETURNING \"n\"",
                30_000.0,
                None,
                none
            ),
        ],
        [
            "An update of sessions runs 30K times in 90 minutes, one row each time · Medium · Medium\n\
                Its runs are 93% of all statement runs, and each run changes about 1 row. Frequent small updates can be independent requests, row-by-row loops, heartbeats or status changes; each call can add a round trip, and each committing transaction can flush the log (9% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 93% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Where the application changes many rows the same way, update them in one statement (an UPDATE over a list of keys (`IN (…)`), or joined to a temporary table of the changes) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 90 minutes; after the change it should run fewer times and take less time in all while the useful work stays the same.\n\
                caveat: Some frequent single-row updates are legitimate (a status that changes as its work progresses). Check the calling code before batching.",
            "A delete from sessions runs 30K times in 90 minutes, one row each time · Medium · Medium\n\
                Its runs are 93% of all statement runs, and each run changes about 1 row. Frequent small deletes can be independent removals or a row-by-row loop; each call can add a round trip, and each committing transaction can flush the log (9% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 93% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller already has independent removals that can safely be grouped, delete many rows per statement (an DELETE over a list of keys (`IN (…)`), or joined to a temporary table of the changes, or by a condition the rows share) inside one transaction.\n\
                verify: It ran 30K times in 90 minutes; after the change it should run fewer times and take less time in all while the useful work stays the same.\n\
                caveat: Some frequent single-row deletes are legitimate (a row removed as its work completes). Check the calling code before batching.",
            "An insert into events runs 30K times in 90 minutes, one row each time · Medium · Medium\n\
                Its runs are 93% of all statement runs, and each run changes about 1 row. Frequent small writes can be individual events, heartbeats or a row-by-row loop; each call can add a round trip, and each committing transaction can flush the log (9% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 93% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller already has independent rows that can safely be grouped, write many rows per statement (a multi-row VALUES) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran 30K times in 90 minutes; after the change it should run fewer times and take less time in all while the useful work stays the same.\n\
                caveat: Some frequent single-row writes are legitimate (each event recorded as it happens). Check the calling code before batching.",
            "An update of counters runs 30K times in 90 minutes, one row each time · Medium · Medium\n\
                Its runs are 93% of all statement runs. Each run adds to n in one row of counters and returns the new value. It may allocate numbers or maintain other application state; the SQL alone does not establish its business purpose or a removable call. Each call can add a round trip, and the row stays locked until its transaction commits, so callers that need the next number of the same row can wait for each other. In all it takes 9% of database time.\n\
                impact: 93% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: The SQL does not establish an isolated counter-only operation: preserve every assignment, returned value, row filter and transaction guarantee. Check the caller before changing it. If it already has independent work that can safely share a call, consider batching that work; per-parent numbering and metadata updates may require this atomic UPDATE. A sequence is not a replacement for the whole statement.\n\
                verify: It ran 30K times in 90 minutes; after the change compare total time and any avoidable calls, while checking that numbering, metadata updates and returned values remain correct.\n\
                caveat: A sequence can have gaps, allocates outside transaction commit order and does not provide per-parent numbering. Check the application's numbering and atomicity requirements before considering one.",
        ]
    );
    // Runs a day without Culprit's scans (two days of Query Store), shares of the runs Culprit's scans and the totals
    // put twofold apart, and SQLite's own counter.
    let mut daily = mssql();
    crate::last_hours(daily.workload.as_mut().unwrap(), 48.0);
    daily.workload.as_mut().unwrap().timeline = None;
    let start = daily.workload.as_ref().unwrap().window_start.clone();
    let steady = |q: &mut culprit_core::QueryStat| {
        (q.first_seen, q.last_seen, q.trend) = (start, Some("2026-10-11T12:13:00Z".into()), None);
        (q.executions, q.total_time_ms, q.rows) = (60_000.0, 60_000.0 * 0.13, Some(60_000.0));
    };
    assert_eq!(
        [
            said(daily, "7", read, 30_000.0, None, steady),
            scanned(1_200_000.0).expect("a chatty lookup Culprit's scans measured"),
            said(
                demo("sqlite-demo.json"),
                sqlite,
                "UPDATE counters SET n = n + 1 WHERE name = ? RETURNING n",
                30_000.0,
                None,
                none
            ),
        ],
        [
            "A read from customers runs about 30K times a day, one key at a time · Low · Medium\n\
                Its runs are 47% of all statement runs, and each run returns about 1 row. Repeated lookups can be per-request checks or a loop in application code (N+1). Each run is cheap (19% of database time in all), but each call can add a network round trip.\n\
                impact: 47% of executions\n\
                metric: Runs per day\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Reuse a result the application already has, or cache it for as long as it remains valid; access checks must still reflect permission changes. If the caller loops over items, load their rows in one query (WHERE key IN (…) or a join) before the loop; ORMs usually have an eager-loading option for this.\n\
                verify: It ran about 30K times a day; after the change it should run fewer times and take less time in all.\n\
                caveat: Some high-frequency single-row lookups are legitimate (for example, by primary key on a hot path). Check the calling code before batching.",
            "A read from customers runs about 40K times a day over 36 hours of Culprit's scans, one key at a time · Low · Medium\n\
                Its runs are 50% of all statement runs over 36 hours of Culprit's scans, and each run returns about 1 row (counted over the 40 days since statistics were reset). Repeated lookups can be per-request checks or a loop in application code (N+1). Each run is cheap (4% of database time in all), but each call can add a network round trip.\n\
                impact: 50% of executions\n\
                metric: Runs per day over Culprit's scans\n\
                metric: Runs per day over the 40 days since statistics were reset\n\
                metric: Share of executions over Culprit's scans\n\
                metric: Share of executions over the 40 days since statistics were reset\n\
                metric: Average duration over Culprit's scans\n\
                fix: Reuse a result the application already has, or cache it for as long as it remains valid; access checks must still reflect permission changes. If the caller loops over items, load their rows in one query (WHERE key IN (…) or a join) before the loop; ORMs usually have an eager-loading option for this.\n\
                verify: It ran about 40K times a day over 36 hours of Culprit's scans. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some high-frequency single-row lookups are legitimate (for example, by primary key on a hot path). Check the calling code before batching.",
            "An update of counters runs 30K times in 90 minutes, one row each time · Medium · Medium\n\
                Its runs are 93% of all statement runs. Each run adds to n in one row of counters and returns the new value. It may allocate numbers or maintain other application state; the SQL alone does not establish its business purpose or a removable call. Each call can add a round trip, and the row stays locked until its transaction commits, so callers that need the next number of the same row can wait for each other. In all it takes 9% of database time.\n\
                impact: 93% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: If the caller confirms that this is solely a number allocator, consider an INTEGER PRIMARY KEY only if a database-wide allocator, gaps and allocation order satisfy the application's requirements. It does not preserve gapless or per-parent numbering or commit order. Otherwise keep the atomic UPDATE. Check whether the caller can combine work without losing filters, returned values or transaction guarantees; the SQL alone does not establish a removable call.\n\
                verify: It ran 30K times in 90 minutes; after the change compare total time and any avoidable calls, while checking that numbering, metadata updates and returned values remain correct.\n\
                caveat: A sequence can have gaps, allocates outside transaction commit order and does not provide per-parent numbering. Check the application's numbering and atomicity requirements before considering one.",
        ]
    );
    // What Culprit's scans measured of a write of a few rows, a poll, a write that changes nothing and a counter; and a
    // write that changes nothing without them.
    let scanned_as = |text: &str, rows: f64| scanned_as(0.0, Some(text), rows).expect("a chatty statement");
    assert_eq!(
        [
            scanned_as("UPDATE sessions SET seen_at = now() WHERE user_id = $1", 640_000.0),
            scanned_as("SELECT id FROM jobs WHERE status = $1", 0.0),
            scanned_as("UPDATE jobs SET seen_at = now() WHERE id = $1 AND owner IS NULL", 0.0),
            scanned_as("UPDATE counters SET n = n + 1 WHERE name = $1 RETURNING n", 400_000.0),
            said(
                demo("postgres-demo.json"),
                pg,
                "UPDATE jobs SET seen_at = now() WHERE id = $1 AND owner IS NULL",
                0.0,
                None,
                none
            ),
        ],
        [
            "An update of sessions runs about 40K times a day over 36 hours of Culprit's scans, about 1.6 rows each time · Low · Medium\n\
                Its runs are 50% of all statement runs over 36 hours of Culprit's scans, and each run changes about 1.6 rows (counted over the 40 days since statistics were reset). Frequent small updates can be independent requests, row-by-row loops, heartbeats or status changes; each call can add a round trip, and each committing transaction can flush the log (4% of database time in all). The counters do not show whether calls already share a transaction.\n\
                impact: 50% of executions\n\
                metric: Runs per day over Culprit's scans\n\
                metric: Runs per day over the 40 days since statistics were reset\n\
                metric: Share of executions over Culprit's scans\n\
                metric: Average duration over Culprit's scans\n\
                fix: Where the application changes many rows the same way, update them in one statement (an UPDATE over an array of keys (`= ANY($1)`), or joined to a VALUES list) inside one transaction; for a heartbeat or lease, renew less often.\n\
                verify: It ran about 40K times a day over 36 hours of Culprit's scans. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent single-row updates are legitimate (a status that changes as its work progresses). Check the calling code before batching.",
            "A read from jobs runs about 40K times a day over 36 hours of Culprit's scans and most runs find nothing · Low · Medium\n\
                Its runs are 50% of all statement runs over 36 hours of Culprit's scans, and on average a run returns 0 rows (counted over the 40 days since statistics were reset). Empty results can come from polling, request-driven checks or legitimate misses; the counters do not identify the caller or prove a timer. Each run is cheap (4% of database time in all), but unnecessary calls still add round trips.\n\
                impact: 50% of executions\n\
                metric: Runs per day over Culprit's scans\n\
                metric: Runs per day over the 40 days since statistics were reset\n\
                metric: Share of executions over Culprit's scans\n\
                metric: Average duration over Culprit's scans\n\
                fix: Check the caller first. If it polls for work, back off after empty results, coordinate pollers, or have the writer wake them (a queue or notification). For request-driven checks, reuse an answer only while it remains valid. Keep necessary checks and their correctness guarantees.\n\
                verify: It ran about 40K times a day over 36 hours of Culprit's scans. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there unnecessary calls and total database time should fall while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent checks are legitimate (a lease or a lock checked on every request). Check the calling code before changing how often it asks.",
            "An update of jobs runs about 40K times a day over 36 hours of Culprit's scans and most runs find nothing · Low · Medium\n\
                Its runs are 50% of all statement runs over 36 hours of Culprit's scans, and on average a run changes 0 rows (counted over the 40 days since statistics were reset). Empty results can come from polling, request-driven checks or legitimate misses; the counters do not identify the caller or prove a timer. Each run is cheap (4% of database time in all), but unnecessary calls still add round trips.\n\
                impact: 50% of executions\n\
                metric: Runs per day over Culprit's scans\n\
                metric: Runs per day over the 40 days since statistics were reset\n\
                metric: Share of executions over Culprit's scans\n\
                metric: Average duration over Culprit's scans\n\
                fix: Check the caller first. If it polls for work, back off after empty results, coordinate pollers, or have the writer wake them (a queue or notification). For request-driven checks, reuse an answer only while it remains valid. Keep necessary checks and their correctness guarantees.\n\
                verify: It ran about 40K times a day over 36 hours of Culprit's scans. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there unnecessary calls and total database time should fall while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent checks are legitimate (a lease or a lock checked on every request). Check the calling code before changing how often it asks.",
            "An update of counters runs about 40K times a day over 36 hours of Culprit's scans, one row each time · Low · Medium\n\
                Its runs are 50% of all statement runs over 36 hours of Culprit's scans. Each run adds to n in one row of counters and returns the new value. It may allocate numbers or maintain other application state; the SQL alone does not establish its business purpose or a removable call. Each call can add a round trip, and the row stays locked until its transaction commits, so callers that need the next number of the same row can wait for each other. In all it takes 4% of database time.\n\
                impact: 50% of executions\n\
                metric: Runs per day over Culprit's scans\n\
                metric: Runs per day over the 40 days since statistics were reset\n\
                metric: Share of executions over Culprit's scans\n\
                metric: Average duration over Culprit's scans\n\
                fix: If the caller confirms that this is solely a number allocator, consider a sequence (`nextval()`, or an identity column) only if a database-wide allocator, gaps and allocation order satisfy the application's requirements. It does not preserve gapless or per-parent numbering or commit order. Otherwise keep the atomic UPDATE. Check whether the caller can combine work without losing filters, returned values or transaction guarantees; the SQL alone does not establish a removable call.\n\
                verify: It ran about 40K times a day over 36 hours of Culprit's scans. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare total time and any avoidable calls, while checking that numbering, metadata updates and returned values remain correct. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: A sequence can have gaps, allocates outside transaction commit order and does not provide per-parent numbering. Check the application's numbering and atomicity requirements before considering one.",
            "An update of jobs runs 30K times in 5 hours and most runs find nothing · Medium · Medium\n\
                Its runs are 75% of all statement runs, and on average a run changes 0 rows. Empty results can come from polling, request-driven checks or legitimate misses; the counters do not identify the caller or prove a timer. Each run is cheap (2% of database time in all), but unnecessary calls still add round trips.\n\
                impact: 75% of executions\n\
                metric: Share of executions\n\
                metric: Average duration\n\
                fix: Check the caller first. If it polls for work, back off after empty results, coordinate pollers, or have the writer wake them (a queue or notification). For request-driven checks, reuse an answer only while it remains valid. Keep necessary checks and their correctness guarantees.\n\
                verify: It ran 30K times in 5 hours. pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there unnecessary calls and total database time should fall while the useful work stays the same. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.\n\
                caveat: Some frequent checks are legitimate (a lease or a lock checked on every request). Check the calling code before changing how often it asks.",
        ]
    );
}
