//! What the waits explanation says, word for word: where its numbers come from, the notes about what each engine
//! measures, and the sentence under each category. Its words come from the language catalog (locales/en.json); these
//! tests hold the English to what it said when it wrote it in code.

use culprit_checks::waits::explain;
use culprit_core::{
    Capability, Engine, GrantStat, Platform, Signal, SignalScope, SignalUnit, Snapshot, WaitStat, WaitsReport,
};

fn snapshot(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    let mut s: Snapshot = serde_json::from_str(&text).expect("snapshot");
    if let Some(w) = s.workload.as_mut() {
        w.queries.iter_mut().for_each(|q| q.waits.clear());
    }
    s
}

fn wait(category: &str, ms: f64) -> WaitStat {
    WaitStat { category: category.into(), wait_type: Some(category.into()), wait_ms: ms, tasks: None }
}

/// Everything the report says in words, one line a part.
fn said(report: &WaitsReport) -> String {
    let mut out = vec![format!("source: {}", report.source)];
    out.extend(report.notes.iter().map(|n| format!("note: {n}")));
    for c in &report.categories {
        out.push(format!("{}: {}", c.key, c.detail.as_deref().unwrap_or("-")));
    }
    out.join("\n")
}

#[track_caller]
fn same(actual: &str, expected: &str) {
    assert!(actual == expected, "said:\n{actual}\n\nwhere the English was:\n{expected}");
}

fn set(s: &mut Snapshot, name: &str, value: &str) {
    for x in s.settings.iter_mut().filter(|x| x.name == name) {
        x.value = value.into();
    }
}

/// SQL Server's Query Store waits, tied to queries: a sentence for each category.
#[test]
fn sql_server_categories_say_what_their_queries_did() {
    let mut s = snapshot("mssql-demo.json");
    s.waits = ["Network", "Lock", "Memory", "Buffer IO", "Parallelism", "Compilation", "Latch", "CPU"]
        .iter()
        .enumerate()
        .map(|(i, c)| wait(c, 1000.0 - i as f64 * 100.0))
        .collect();
    let hash = s.workload.as_ref().unwrap().queries[2].query_hash.clone().unwrap();
    s.pressure.get_or_insert_with(Default::default).grants.push(GrantStat {
        query_hash: hash,
        executions: 10.0,
        avg_granted_kb: 204_800.0,
        avg_used_kb: 1024.0,
        max_granted_kb: 204_800.0,
        last_granted_kb: None,
        last_used_kb: None,
    });
    let w = s.workload.as_mut().unwrap();
    w.queries[0].rows = Some(w.queries[0].executions * 80_000.0);
    w.queries[0].waits = vec![wait("Network", 900.0), wait("Lock", 50.0)];
    w.queries[1].waits =
        vec![wait("Network", 100.0), wait("Lock", 750.0), wait("Buffer IO", 700.0), wait("Parallelism", 500.0)];
    w.queries[2].waits = vec![wait("Memory", 800.0), wait("Compilation", 400.0), wait("Latch", 300.0)];
    same(
        &said(&explain(&s).unwrap()),
        r#"source: Query Store, last 5 hours
network: 1 query returns 80K+ rows per call: page or filter them, or stream them without per-row work in the application.
lock: A read from order_items accounts for 83% of it. Live shows who blocks whom right now.
memory: A read from support_tickets accounts for 100% of it; it reserves 200 MB per run and uses 1.0 MB.
io: A read from order_items accounts for 100% of it, reading 13.1 pages from disk per run.
parallelism: A read from order_items accounts for 83% of it. Check its plan for a large scan an index would avoid.
compilation: A read from support_tickets accounts for 80% of it.
latch: A read from support_tickets accounts for 75% of it.
cpu: -"#,
    );
}

/// The other shapes: results that aren't big, several big ones, a grant not found, no reads from disk, no compiling
/// statement, and grants pending with no statement to blame.
#[test]
fn sql_server_categories_say_what_else_their_numbers_show() {
    let mut s = snapshot("mssql-demo.json");
    s.waits =
        vec![wait("Network", 1000.0), wait("Memory", 800.0), wait("Buffer IO", 700.0), wait("Compilation", 600.0)];
    let w = s.workload.as_mut().unwrap();
    w.queries[0].rows = Some(w.queries[0].executions * 12.0);
    w.queries[0].waits = vec![wait("Network", 900.0), wait("Memory", 700.0)];
    w.queries[3].physical_reads = Some(0.0);
    w.queries[3].waits = vec![wait("Buffer IO", 700.0)];
    same(
        &said(&explain(&s).unwrap()),
        r#"source: Query Store, last 5 hours
network: A read from customers accounts for 90% of it; it returns 12 rows per call, so look at how the application reads them.
memory: A read from customers accounts for 88% of it.
io: A read from orders accounts for 100% of it.
compilation: Compiling is a sign of one-off statements; see the compile findings."#,
    );

    let mut s = snapshot("mssql-demo.json");
    s.waits = vec![wait("Network", 1000.0), wait("Memory", 800.0)];
    s.pressure.get_or_insert_with(Default::default).signals.push(Signal::new(
        culprit_core::signal_id::MEMORY_GRANTS_PENDING,
        "Grants pending",
        3.0,
        SignalUnit::Count,
        SignalScope::Now,
    ));
    let w = s.workload.as_mut().unwrap();
    for q in w.queries.iter_mut().take(2) {
        q.rows = Some(q.executions * 50_000.0);
        q.waits = vec![wait("Network", 500.0)];
    }
    same(
        &said(&explain(&s).unwrap()),
        r#"source: Query Store, last 5 hours
network: 2 queries return 50K+ rows per call: page or filter them, or stream them without per-row work in the application.
memory: 3 queries were waiting for memory when the scan ran."#,
    );
}

/// Server totals since startup, and what Query Store would need to tie them to queries.
#[test]
fn sql_server_totals_say_why_they_are_not_per_query() {
    let mut s = snapshot("mssql-demo.json");
    s.waits = vec![wait("CPU", 100.0)];
    let note = |s: &Snapshot| said(&explain(s).unwrap());
    let mut out = vec![note(&s)];
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(300.0);
    set(&mut s, "query_store_options.wait_stats_capture_mode_desc", "OFF");
    out.push(note(&s));
    set(&mut s, "query_store_options.actual_state_desc", "OFF");
    out.push(note(&s));
    s.server.version = "13.0.6300.2".into();
    s.server.product = "SQL Server 2016".into();
    out.push(note(&s));
    same(
        &out.join("\n--\n"),
        r#"source: Server totals since SQL Server started
note: Query Store recorded no waits for this database's queries in the window. These are server totals since startup and can't be tied to queries.
cpu: -
--
source: Server totals since SQL Server started 13 days ago
note: Query Store isn't capturing waits. These are server totals since startup and can't be tied to queries. Turn it on: ALTER DATABASE CURRENT SET QUERY_STORE (WAIT_STATS_CAPTURE_MODE = ON);
cpu: -
--
source: Server totals since SQL Server started 13 days ago
note: These are server totals since startup and can't be tied to queries. Query Store is off; with it on (and recording waits, the default from SQL Server 2017), Culprit can tie waits to queries: ALTER DATABASE CURRENT SET QUERY_STORE = ON (OPERATION_MODE = READ_WRITE, WAIT_STATS_CAPTURE_MODE = ON);
cpu: -
--
source: Server totals since SQL Server started 13 days ago
note: These are server totals since startup and can't be tied to queries. Query Store on SQL Server 2016 doesn't record waits per query; SQL Server 2017 and later do.
cpu: -"#,
    );
}

#[test]
fn postgres_says_where_its_timing_comes_from() {
    let mut s = snapshot("postgres-demo.json");
    s.waits = vec![wait("Other", 1000.0)];
    let mut out = vec![said(&explain(&s).unwrap())];
    set(&mut s, "track_io_timing", "off");
    out.push(said(&explain(&s).unwrap()));
    s.server.set_platform(Some(Platform::AmazonRds));
    out.push(said(&explain(&s).unwrap()));
    s.workload = None;
    out.push(said(&explain(&s).unwrap()));
    same(
        &out.join("\n--\n"),
        r#"source: Statement timing from pg_stat_statements, the 5 hours since statistics were reset
note: PostgreSQL keeps no history of what sessions waited on. These totals come from per-statement timing; use Right now to sample what sessions wait on at this moment.
other: -
--
source: Statement timing from pg_stat_statements, the 5 hours since statistics were reset
note: PostgreSQL keeps no history of what sessions waited on. These totals come from per-statement timing; use Right now to sample what sessions wait on at this moment.
note: Turn on track_io_timing so time spent reading from disk is measured per statement. Set track_io_timing to on and reload the configuration (settings marked as needing a restart take effect after one). (ALTER SYSTEM SET track_io_timing = 'on'; SELECT pg_reload_conf();)
other: -
--
source: Statement timing from pg_stat_statements, the 5 hours since statistics were reset
note: PostgreSQL keeps no history of what sessions waited on. These totals come from per-statement timing; use Right now to sample what sessions wait on at this moment.
note: Turn on track_io_timing so time spent reading from disk is measured per statement. Set track_io_timing to on in the instance's DB parameter group.
other: -
--
source: Statement timing from pg_stat_statements
note: PostgreSQL keeps no history of what sessions waited on. These totals come from per-statement timing; use Right now to sample what sessions wait on at this moment.
note: Turn on track_io_timing so time spent reading from disk is measured per statement. Set track_io_timing to on in the instance's DB parameter group.
other: -"#,
    );
}

#[test]
fn mysql_says_what_performance_schema_counts() {
    let mut s = snapshot("mysql-demo.json");
    s.waits = vec![wait("Table IO", 900.0), wait("Lock", 100.0)];
    let mut out = vec![said(&explain(&s).unwrap())];
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(5.0);
    out.push(said(&explain(&s).unwrap()));
    s.server.capabilities.retain(|c| *c != Capability::PerformanceSchema);
    out.push(said(&explain(&s).unwrap()));
    same(
        &out.join("\n--\n"),
        r#"source: performance_schema totals since MySQL started
note: MySQL counts waits since it started, only for enabled performance_schema instruments. Lock time is attributed to statements; the rest is a server-wide total.
rows: -
lock: -
--
source: performance_schema totals since MySQL started 5 hours ago
note: MySQL counts waits since it started, only for enabled performance_schema instruments. Lock time is attributed to statements; the rest is a server-wide total.
rows: -
lock: -
--
source: Lock time per statement from the slow query log, the last 2 minutes
note: performance_schema is off, so MySQL times no waits; the slow query log keeps each statement's lock time. Right now samples what sessions are doing.
rows: -
lock: -"#,
    );
}

#[test]
fn yugabyte_and_sqlite_say_what_they_keep() {
    let mut s = snapshot("yugabyte-demo.json");
    s.waits = vec![wait("RPC", 100.0)];
    let mut out = vec![said(&explain(&s).unwrap())];
    let mut s = snapshot("postgres-demo.json");
    s.server.engine = Engine::Sqlite;
    s.waits = vec![wait("Other", 100.0)];
    out.push(said(&explain(&s).unwrap()));
    same(
        &out.join("\n--\n"),
        r#"source: YSQL native metrics in their recorded node/cluster scope
note: DocDB storage timings and RPC times can overlap; they are not exclusive shares of statement time. Use native metrics or sample YSQL sessions.
rpc: -
--
source: SQLite keeps no waits
other: -"#,
    );
}

/// A sample of recorded activity, with no waits: where it came from and when it was taken.
#[test]
fn sampled_activity_says_what_it_retained() {
    let mut s = snapshot("yugabyte-demo.json");
    s.waits.clear();
    let mut period = culprit_core::ActivityPeriod {
        source_message: None,
        note_messages: None,

        source: "Native recorded activity".into(),
        requested_start: "2026-10-01T00:00:00Z".into(),
        requested_end: "2026-10-08T00:00:00Z".into(),
        first_sample: Some("2026-10-07T00:00:00Z".into()),
        last_sample: Some("2026-10-08T00:00:00Z".into()),
        scope: culprit_core::CounterScope::Node,
        nodes: vec!["recorded-node".into()],
        observations: 1.0,
        weighted_observations: 1.0,
        partial: false,
        notes: vec![],
    };
    let event = culprit_core::ActivityEvent {
        category: "cpu".into(),
        component: "YSQL".into(),
        wait_event: "OnCpu_Active".into(),
        query_id: None,
        node: Some("recorded-node".into()),
        observations: 1.0,
        weighted_observations: 1.0,
    };
    s.operations.activity_history =
        Some(culprit_core::ActivityHistory { period: period.clone(), events: vec![event.clone()] });
    let mut out = vec![said(&explain(&s).unwrap())];
    (period.first_sample, period.last_sample) = (None, None);
    s.operations.activity_history = Some(culprit_core::ActivityHistory { period, events: vec![event] });
    out.push(said(&explain(&s).unwrap()));
    same(
        &out.join("\n--\n"),
        r#"source: Native recorded activity; retained observations 2026-10-07T00:00:00Z to 2026-10-08T00:00:00Z
--
source: Native recorded activity; retained observations unavailable to unavailable"#,
    );
}
