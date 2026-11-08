//! What the dashboard's tiles say, word for word, in cases the demo sidecars (which the engine's invariants compare)
//! don't reach. Their words come from the language catalog (locales/en.json); these tests hold the English to what the
//! tiles said when they wrote it in code.

use culprit_checks::analyze;
use culprit_checks::dashboard::{dashboard, refresh, with_now};
use culprit_core::{
    Dashboard, DeadlockReport, Engine, LiveState, Report, Session, Signal, SignalStatus, Snapshot, Tile, TileKey,
    Timeline,
};
use serde_json::json;

fn report(name: &str) -> Report {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("report")
}

fn snapshot(name: &str) -> Snapshot {
    report(name).snapshot
}

#[track_caller]
fn same(actual: &str, expected: &str) {
    assert!(actual == expected, "said:\n{actual}\n\nwhere the English was:\n{expected}");
}

/// Everything a tile says, one line a part.
fn words(t: &Tile) -> String {
    let mut out = vec![format!("{:?} {:?} {}: {}", t.key, t.status, t.title, t.takeaway)];
    if let Some(d) = &t.detail {
        out.push(format!("  detail: {d}"));
    }
    out.extend(t.metrics.iter().map(|m| format!("  metric {}: {}", m.label, m.text)));
    out.extend(t.items.iter().map(|i| format!("  item {}: {}", i.label, i.value.as_deref().unwrap_or("-"))));
    out.join("\n")
}

fn tile(d: &Dashboard, key: TileKey) -> String {
    words(d.tile(key).expect("a tile"))
}

/// The tile `key` of the dashboard of `s`, analyzed.
fn said(s: Snapshot, key: TileKey) -> String {
    tile(&dashboard(&analyze(s)), key)
}

/// The tile `key` of the dashboard of the saved report `name` with its snapshot replaced by `s`, as saved: its findings
/// and graded signals stay as they were.
fn as_saved(name: &str, s: Snapshot, key: TileKey) -> String {
    let mut r = report(name);
    r.snapshot = s;
    tile(&dashboard(&r), key)
}

/// A snapshot saved as it was collected, not a report.
fn raw(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

/// `s` with its JSON changed by `edit`: for the parts of a snapshot built from many fields.
fn edited(s: &Snapshot, edit: impl FnOnce(&mut serde_json::Value)) -> Snapshot {
    let mut v = serde_json::to_value(s).unwrap();
    edit(&mut v);
    serde_json::from_value(v).expect("a snapshot")
}

fn timeline(w: &mut culprit_core::Workload, end: &str, hours: usize, time_ms: Vec<f64>) {
    let end_s = culprit_core::format::epoch_seconds(end).unwrap();
    let n = time_ms.len();
    let starts: Vec<String> =
        (0..n).map(|k| culprit_core::format::iso_from_epoch(end_s - ((n - k) * hours * 3600) as f64)).collect();
    w.window_start = Some(starts[0].clone());
    w.timeline = Some(Timeline {
        source: Default::default(),
        starts,
        end: end.into(),
        time_ms,
        executions: vec![1.0; n],
        unknown: None,
    });
}

#[test]
fn database_time_and_its_spans_say_what_they_said() {
    let mut out = Vec::new();
    let base = snapshot("mssql-demo.report.json");
    // Two days of history: said a day, steady or without a timeline.
    let mut s = base.clone();
    let end = s.workload.as_ref().unwrap().window_end.clone().unwrap();
    crate::last_hours(s.workload.as_mut().unwrap(), 48.0);
    timeline(s.workload.as_mut().unwrap(), &end, 6, vec![10_000.0; 8]);
    out.push(said(s.clone(), TileKey::DbTime));
    s.workload.as_mut().unwrap().timeline = None;
    out.push(said(s, TileKey::DbTime));
    // Up from almost nothing, down, and up a lot over hours; a running total with no trend yet.
    for pattern in [
        vec![100.0, 100.0, 100.0, 100.0, 100.0, 100.0, 300.0, 300.0],
        vec![30_000.0, 30_000.0, 30_000.0, 30_000.0, 30_000.0, 30_000.0, 10_000.0, 10_000.0],
        vec![10_000.0, 10_000.0, 10_000.0, 10_000.0, 10_000.0, 10_000.0, 20_000.0, 20_000.0],
    ] {
        let mut s = base.clone();
        timeline(s.workload.as_mut().unwrap(), &end, 1, pattern);
        out.push(said(s, TileKey::DbTime));
    }
    // More than one query switched plans: each of those that did is run again under another id.
    let mut s = raw("mssql-demo.json");
    let w = s.workload.as_mut().unwrap();
    let switched: Vec<_> = w
        .queries
        .iter()
        .filter(|q| q.workload_plans().len() >= 2)
        .map(|q| {
            let mut q = q.clone();
            q.id = format!("{}-again", q.id);
            q
        })
        .collect();
    w.queries.extend(switched);
    out.push(said(s, TileKey::DbTime));
    // Spans: no deadlocks over a day, days, hours and minutes.
    for hours in [24.0, 72.0, 30.0, 5.0, 1.0, 0.5, 0.01] {
        let mut s = base.clone();
        s.deadlocks = Some(DeadlockReport { source: "system_health".into(), since_hours: hours, ..Default::default() });
        out.push(said(s, TileKey::Deadlocks));
    }
    same(&out.join("\n"), DB_TIME);
}

const DB_TIME: &str = r#"DbTime Good Database time: Database time is steady at about 3 min a day.
  metric Database time per day: 3 min
  metric Runs per day: 36K
  metric On CPU: 27%
  metric Covers: 2 days
DbTime Info Database time: About 3 min of database time a day over the last 2 days.
  metric Database time per day: 3 min
  metric Runs per day: 36K
  metric On CPU: 27%
  metric Covers: 2 days
DbTime Info Database time: Database time is up 3.0× in the last 2 hours, but small: 6 min over the recorded workload window.
  metric Database time: 6 min
  metric Runs: 71K
  metric On CPU: 27%
  metric Covers: 1 minute
DbTime Good Database time: Database time is down 67% in the last 2 hours compared with the 6 hours before.
  metric Database time: 6 min
  metric Runs: 71K
  metric On CPU: 27%
  metric Covers: 1 minute
DbTime Watch Database time: Database time is up 2.0× in the last 2 hours compared with the 6 hours before.
  metric Database time: 6 min
  metric Runs: 71K
  metric On CPU: 27%
  metric Covers: 1 minute
DbTime Good Database time: Database time is steady: 40.7 s over the last 5 hours.
  detail: 2 queries switched plans in this window; the busiest is a read from orders (12% of database time).
  metric Database time: 40.7 s
  metric Runs: 70K
  metric On CPU: 100%
  metric Covers: 5 hours
  item a read from orders: 12%
  item a read from orders: 12%
Deadlocks Good Deadlocks: No deadlocks in the last day.
  metric Deadlocks: 0
Deadlocks Good Deadlocks: No deadlocks in the last 3 days.
  metric Deadlocks: 0
Deadlocks Good Deadlocks: No deadlocks in the last 30 hours.
  metric Deadlocks: 0
Deadlocks Good Deadlocks: No deadlocks in the last 5 hours.
  metric Deadlocks: 0
Deadlocks Good Deadlocks: No deadlocks in the last 60 minutes.
  metric Deadlocks: 0
Deadlocks Good Deadlocks: No deadlocks in the last 30 minutes.
  metric Deadlocks: 0
Deadlocks Good Deadlocks: No deadlocks in the last 1 minute.
  metric Deadlocks: 0"#;

fn deadlock_sample() -> DeadlockReport {
    let text = std::fs::read_to_string(format!("{}/../../fixtures/deadlocks.sample.json", env!("CARGO_MANIFEST_DIR")))
        .expect("fixture");
    let all: serde_json::Value = serde_json::from_str(&text).expect("json");
    serde_json::from_value(all["mssql"].clone()).expect("deadlock report")
}

#[test]
fn deadlocks_say_what_they_said() {
    let mut out = Vec::new();
    let sample = deadlock_sample();
    let (opposite, lookup) = (sample.deadlocks[0].clone(), sample.deadlocks[1].clone());
    let at = |d: &culprit_core::Deadlock, n: usize| {
        let mut d = d.clone();
        d.id = format!("{}-{n}", d.id);
        d.at = Some(format!("2026-10-11T13:{:02}:00Z", 10 + n));
        d
    };
    let cases: Vec<Vec<culprit_core::Deadlock>> = vec![
        vec![at(&lookup, 1), at(&lookup, 2), at(&lookup, 3)],
        vec![at(&lookup, 1), at(&opposite, 2)],
        vec![at(&lookup, 1), at(&lookup, 2), at(&opposite, 3)],
        vec![at(&lookup, 1)],
    ];
    for objects in [true, false] {
        for case in &cases {
            let mut s = snapshot("mssql-demo.report.json");
            s.taken_at = "2026-10-11T14:00:00Z".into();
            let mut d = sample.clone();
            d.deadlocks = case.clone();
            if !objects {
                d.deadlocks.iter_mut().for_each(|d| d.objects.clear());
            }
            s.deadlocks = Some(d);
            out.push(said(s, TileKey::Deadlocks));
        }
    }
    // Deadlocks a server counted, with and without the reset they count from.
    for (counted, since) in [(0.0, true), (0.0, false), (1.0, false), (3.0, false)] {
        let mut s = snapshot("pg-demo.report.json");
        s.deadlocks = Some(DeadlockReport {
            source: "pg_stat_database".into(),
            since_hours: 24.0,
            counted: Some(counted),
            counted_since: since.then(|| "2026-10-05T12:00:00Z".into()),
            ..Default::default()
        });
        out.push(said(s, TileKey::Deadlocks));
    }
    same(&out.join("\n"), DEADLOCKS);
}

const DEADLOCKS: &str = r#"Deadlocks Watch Deadlocks: 3 deadlocks in the last 6 hours, all key-lookup deadlock on shop.customers.
  detail: Read from system_health event files and ring buffer.
  metric Deadlocks: 3
  item Key-lookup deadlock on shop.customers: 3×
Deadlocks Info Deadlocks: 2 deadlocks in the last 6 hours, each a different kind.
  detail: Read from system_health event files and ring buffer.
  metric Deadlocks: 2
  item Key-lookup deadlock on shop.customers: 1×
  item Opposite access order on shop.customers, shop.orders: 1×
Deadlocks Info Deadlocks: 3 deadlocks in the last 6 hours; 2 of them the same key-lookup deadlock on shop.customers.
  detail: Read from system_health event files and ring buffer.
  metric Deadlocks: 3
  item Key-lookup deadlock on shop.customers: 2×
  item Opposite access order on shop.customers, shop.orders: 1×
Deadlocks Info Deadlocks: One deadlock in the last 6 hours: key-lookup deadlock on shop.customers.
  detail: Read from system_health event files and ring buffer.
  metric Deadlocks: 1
  item Key-lookup deadlock on shop.customers: 1×
Deadlocks Watch Deadlocks: 3 deadlocks in the last 6 hours, all key-lookup deadlock.
  detail: Read from system_health event files and ring buffer.
  metric Deadlocks: 3
  item Key-lookup deadlock: 3×
Deadlocks Info Deadlocks: 2 deadlocks in the last 6 hours, each a different kind.
  detail: Read from system_health event files and ring buffer.
  metric Deadlocks: 2
  item Key-lookup deadlock: 1×
  item Opposite access order: 1×
Deadlocks Info Deadlocks: 3 deadlocks in the last 6 hours; 2 of them the same key-lookup deadlock.
  detail: Read from system_health event files and ring buffer.
  metric Deadlocks: 3
  item Key-lookup deadlock: 2×
  item Opposite access order: 1×
Deadlocks Info Deadlocks: One deadlock in the last 6 hours: key-lookup deadlock.
  detail: Read from system_health event files and ring buffer.
  metric Deadlocks: 1
  item Key-lookup deadlock: 1×
Deadlocks Good Deadlocks: No deadlocks counted in the 6.0 days since its statistics were reset.
  metric Counted: 0
Deadlocks Good Deadlocks: No deadlocks counted.
  metric Counted: 0
Deadlocks Info Deadlocks: PostgreSQL counted 1 deadlock; it keeps no details to explain them.
  metric Counted: 1
Deadlocks Info Deadlocks: PostgreSQL counted 3 deadlocks; it keeps no details to explain them.
  metric Counted: 3"#;

#[test]
fn findings_say_what_they_said() {
    let mut out = Vec::new();
    let base = report("mssql-demo.report.json");
    let board = |r: &Report| tile(&dashboard(r), TileKey::Findings);
    // Nothing actionable, over a window and without one.
    let mut r = base.clone();
    r.findings.retain(|f| !f.is_actionable());
    out.push(board(&r));
    r.snapshot.workload = None;
    out.push(board(&r));
    // The top one, two and three, with and without a cost to say.
    for keep in [1, 2, 3] {
        let mut r = base.clone();
        let mut kept = 0;
        r.findings.retain(|f| {
            if !f.is_actionable() {
                return true;
            }
            kept += 1;
            kept <= keep
        });
        out.push(board(&r));
        for f in r.findings.iter_mut() {
            f.impact.time_ms_per_day = None;
        }
        out.push(board(&r));
    }
    // Findings that stopped: one statement behind them, several, and only as old as Culprit's first scan.
    let actionable: Vec<usize> = (0..base.findings.len()).filter(|&k| base.findings[k].is_actionable()).collect();
    for stopped in [1, 2, 4] {
        for last_seen in [base.snapshot.taken_at.clone(), "2026-10-01T00:00:00Z".to_string()] {
            let mut r = base.clone();
            for &k in actionable.iter().take(stopped) {
                r.findings[k].inactive = true;
                r.findings[k].last_seen = Some(last_seen.clone());
            }
            out.push(board(&r));
        }
    }
    same(&out.join("\n"), FINDINGS);
}

const FINDINGS: &str = r#"Findings Good Findings: Nothing stands out over the last 5 hours.
  metric High: 0
  metric Medium: 0
  metric Low: 0
  metric Information and history: 1
Findings Good Findings: Nothing stands out.
  metric High: 0
  metric Medium: 0
  metric Low: 0
  metric Information and history: 1
Findings Bad Findings: 1 opportunity; the top ones touch 2 min of database time over the last 5 hours (27%).
  metric High: 1
  metric Medium: 0
  metric Low: 0
  metric Information and history: 1
  item A read from order_items times out on every run, at about 15 s: 100% of runs fail
Findings Bad Findings: 1 opportunity; the top ones touch 2 min of database time over the last 5 hours (27%).
  metric High: 1
  metric Medium: 0
  metric Low: 0
  metric Information and history: 1
  item A read from order_items times out on every run, at about 15 s: 100% of runs fail
Findings Bad Findings: 2 opportunities; the top ones touch 2 min of database time over the last 5 hours (27%).
  metric High: 2
  metric Medium: 0
  metric Low: 0
  metric Information and history: 1
  item A read from order_items times out on every run, at about 15 s: 100% of runs fail
  item A read from orders fails 25% of the time: 25% of runs fail
Findings Bad Findings: 2 opportunities; the top ones touch 2 min of database time over the last 5 hours (27%).
  metric High: 2
  metric Medium: 0
  metric Low: 0
  metric Information and history: 1
  item A read from order_items times out on every run, at about 15 s: 100% of runs fail
  item A read from orders fails 25% of the time: 25% of runs fail
Findings Bad Findings: 3 opportunities; the top ones touch 2 min of database time over the last 5 hours (28%).
  metric High: 3
  metric Medium: 0
  metric Low: 0
  metric Information and history: 1
  item A read from order_items times out on every run, at about 15 s: 100% of runs fail
  item A read from orders fails 25% of the time: 25% of runs fail
  item Row locks on support_tickets escalated to a table lock 5 times since SQL Server started (Oct 11): 5 table locks
Findings Bad Findings: 3 opportunities; the top ones touch 2 min of database time over the last 5 hours (28%).
  metric High: 3
  metric Medium: 0
  metric Low: 0
  metric Information and history: 1
  item A read from order_items times out on every run, at about 15 s: 100% of runs fail
  item A read from orders fails 25% of the time: 25% of runs fail
  item Row locks on support_tickets escalated to a table lock 5 times since SQL Server started (Oct 11): 5 table locks
Findings Bad Findings: 21 opportunities, 3 of them high.
  detail: 1 more is about a query that stopped running: a deploy may have changed it.
  metric High: 3
  metric Medium: 7
  metric Low: 11
  metric Information and history: 1
  metric Stopped running: 1
  item A read from orders fails 25% of the time: 25% of runs fail
  item Row locks on support_tickets escalated to a table lock 5 times since SQL Server started (Oct 11): 5 table locks
  item Inserts into hot queue for the last page of pk_hot: 53.6 s latch waits
Findings Bad Findings: 21 opportunities, 3 of them high.
  detail: 1 more is about a query that stopped running: a deploy may have changed it.
  metric High: 3
  metric Medium: 7
  metric Low: 11
  metric Information and history: 1
  metric Stopped running: 1
  item A read from orders fails 25% of the time: 25% of runs fail
  item Row locks on support_tickets escalated to a table lock 5 times since SQL Server started (Oct 11): 5 table locks
  item Inserts into hot queue for the last page of pk_hot: 53.6 s latch waits
Findings Bad Findings: 20 opportunities; the top ones touch 3 min of database time over the last 5 hours (58%).
  detail: 2 more are about queries that stopped running: a deploy may have changed them.
  metric High: 2
  metric Medium: 7
  metric Low: 11
  metric Information and history: 1
  metric Stopped running: 2
  item Row locks on support_tickets escalated to a table lock 5 times since SQL Server started (Oct 11): 5 table locks
  item Inserts into hot queue for the last page of pk_hot: 53.6 s latch waits
  item Queries on orders recorded lock waits in the last 5 hours: 3 min waiting on locks
Findings Bad Findings: 20 opportunities; the top ones touch 3 min of database time over the last 5 hours (58%).
  detail: 2 more are about queries that stopped running: a deploy may have changed them.
  metric High: 2
  metric Medium: 7
  metric Low: 11
  metric Information and history: 1
  metric Stopped running: 2
  item Row locks on support_tickets escalated to a table lock 5 times since SQL Server started (Oct 11): 5 table locks
  item Inserts into hot queue for the last page of pk_hot: 53.6 s latch waits
  item Queries on orders recorded lock waits in the last 5 hours: 3 min waiting on locks
Findings Watch Findings: 18 opportunities; the top ones touch 5 min of database time over the last 5 hours (83%).
  detail: 4 more are about queries that stopped running: a deploy may have changed them.
  metric High: 0
  metric Medium: 7
  metric Low: 11
  metric Information and history: 1
  metric Stopped running: 4
  item Queries on orders recorded lock waits in the last 5 hours: 3 min waiting on locks
  item Compares each of 1.5M rows of order_items with 999 rows the Table Spool replays (1.5B pairs): 27% of DB time
  item This plan runs on one CPU because its statement says OPTION (MAXDOP 1): 27% of DB time
Findings Watch Findings: 18 opportunities; the top ones touch 5 min of database time over the last 5 hours (83%).
  detail: 4 more are about queries that stopped running: a deploy may have changed them.
  metric High: 0
  metric Medium: 7
  metric Low: 11
  metric Information and history: 1
  metric Stopped running: 4
  item Queries on orders recorded lock waits in the last 5 hours: 3 min waiting on locks
  item Compares each of 1.5M rows of order_items with 999 rows the Table Spool replays (1.5B pairs): 27% of DB time
  item This plan runs on one CPU because its statement says OPTION (MAXDOP 1): 27% of DB time"#;

#[test]
fn health_says_what_it_said() {
    let mut out = Vec::new();
    // Shared node measurements: several nodes, none usable, a partial read with an unknown count, none read.
    let yb = snapshot("yugabyte-demo.report.json");
    let nodes = |s: &Snapshot, edit: &dyn Fn(&mut serde_json::Value)| {
        edited(s, |v| edit(&mut v["operations"]["node_resources"]))
    };
    out.push(said(
        nodes(&yb, &|r| {
            let node = r["nodes"][0].clone();
            r["nodes"] = json!([node.clone(), node]);
        }),
        TileKey::Health,
    ));
    out.push(said(
        nodes(&yb, &|r| {
            r["nodes"][0]["available"] = json!(false);
            r["partial"] = json!(true);
            r["nodes_expected"] = json!(null);
        }),
        TileKey::Health,
    ));
    let mut s = edited(&yb, |v| v["operations"]["node_resources"] = json!(null));
    crate::record(
        &mut s,
        culprit_core::source_id::DOCDB_NODE_RESOURCES,
        culprit_core::SourceState::Unavailable,
        Some("The login can't read yb_servers_metrics."),
    );
    out.push(said(s, TileKey::Health));
    // Graded signals: all healthy, one and several needing a look, one ungraded alone.
    let pg = snapshot("pg-demo.report.json");
    let graded = |statuses: &[SignalStatus]| -> Snapshot {
        let mut s = pg.clone();
        let p = s.pressure.as_mut().unwrap();
        p.oldest_transaction = None;
        p.signals.truncate(statuses.len());
        for (sig, status) in p.signals.iter_mut().zip(statuses) {
            sig.status = *status;
        }
        s
    };
    use SignalStatus::{Bad, Good, Info, Watch};
    for statuses in
        [&[Good, Good][..], &[Good][..], &[Bad, Watch][..], &[Bad, Bad, Watch][..], &[Info][..], &[Info, Info]]
    {
        out.push(as_saved("pg-demo.report.json", graded(statuses), TileKey::Health));
    }
    let mut s = graded(&[Watch]);
    s.pressure.as_mut().unwrap().signals[0].healthy = None;
    out.push(as_saved("pg-demo.report.json", s, TileKey::Health));
    let mut s = pg.clone();
    s.pressure.as_mut().unwrap().signals.push(Signal::new(
        "extra",
        "Extra",
        5.0,
        culprit_core::SignalUnit::PerHour,
        culprit_core::SignalScope::Now,
    ));
    out.push(said(s, TileKey::Health));
    same(&out.join("\n"), HEALTH);
}

const HEALTH: &str = r#"Health Info Health: Shared resource measurements from 2 nodes. They do not grade this database's pressure.
  detail: yb_servers_metrics · shared YB-TServer nodes · observed 2026-10-24T06:52:49.406861Z to 2026-10-24T06:52:49.917691Z. Host CPU and memory include other databases and processes; tracked allocations keep their native process scope.
  metric Nodes with measurements: 2
  metric Nodes expected: 1
Health Unavailable Health: No node returned usable resource measurements.
  detail: yb_servers_metrics · shared YB-TServer nodes · observed 2026-10-24T06:52:49.406861Z to 2026-10-24T06:52:49.917691Z. Partial read. Host CPU and memory include other databases and processes; tracked allocations keep their native process scope.
  metric Nodes with measurements: 0
  metric Nodes expected: unknown
Health Unavailable Health: Shared node resource measurements were unavailable.
  detail: The login can't read yb_servers_metrics.
Health Good Health: All 2 graded signals look healthy.
  detail: Each signal states the period it measures.
  metric Bad: 0
  metric Watch: 0
  metric Healthy: 2
  item Reads served from memory: 55.29%
  item Temporary files written: 661 MB
Health Good Health: The one graded signal looks healthy.
  detail: Each signal states the period it measures.
  metric Bad: 0
  metric Watch: 0
  metric Healthy: 1
  item Reads served from memory: 55.29%
Health Bad Health: Reads served from memory: 55.29%; Reads served by PostgreSQL's shared_buffers; misses may be served by the operating system cache. The ratio alone does not establish slow storage or a memory shortage.
  detail: 1 more signal also needs a look.
  metric Bad: 1
  metric Watch: 1
  metric Healthy: 0
  item Reads served from memory: 55.29%
  item Temporary files written: 661 MB
Health Bad Health: Reads served from memory: 55.29%; Reads served by PostgreSQL's shared_buffers; misses may be served by the operating system cache. The ratio alone does not establish slow storage or a memory shortage.
  detail: 2 more signals also need a look.
  metric Bad: 2
  metric Watch: 1
  metric Healthy: 0
  item Reads served from memory: 55.29%
  item Temporary files written: 661 MB
  item Commits: 0.7/s
Health Info Health: 1 signal recorded; none has a threshold to grade.
  detail: Each signal states the period it measures.
  metric Bad: 0
  metric Watch: 0
  metric Healthy: 0
  item Reads served from memory: 55.29%
Health Info Health: 2 signals recorded; none has a threshold to grade.
  detail: Each signal states the period it measures.
  metric Bad: 0
  metric Watch: 0
  metric Healthy: 0
  item Reads served from memory: 55.29%
  item Temporary files written: 661 MB
Health Watch Health: Reads served from memory: 55.29%.
  detail: Each signal states the period it measures.
  metric Bad: 0
  metric Watch: 1
  metric Healthy: 0
  item Reads served from memory: 55.29%
Health Info Health: 13 signals recorded; none has a threshold to grade.
  detail: Each signal states the period it measures.
  metric Bad: 0
  metric Watch: 0
  metric Healthy: 0
  item Oldest open transaction: 4.2 s · idle in transaction
  item Reads served from memory: 55.29%
  item Temporary files written: 661 MB
  item Commits: 0.7/s
  item Transactions rolled back: 0.25%
  item Deadlocks: 1
  item Checkpoints (whole server): 32
  item Checkpoints forced early (whole server): 15.62%
  item Connections in use (whole server): 6.19%"#;

#[test]
fn indexes_and_plans_say_what_they_said() {
    let mut out = Vec::new();
    // Indexes: without query history, with one or several to clean up; with one candidate or none.
    for name in ["mssql-demo.report.json", "pg-demo.report.json", "mysql-demo.report.json", "sqlite-demo.report.json"] {
        let base = snapshot(name);
        let mut s = base.clone();
        s.workload = None;
        out.push(said(s, TileKey::Indexes));
        // One candidate left: the others' findings stopped.
        let r = analyze(base.clone());
        let advice = culprit_checks::index_advice(&r);
        let mut r = r.clone();
        let first: Vec<String> = advice.candidates.iter().take(1).filter_map(|c| c.finding_id.clone()).collect();
        for f in r.findings.iter_mut() {
            if matches!(f.check.as_str(), "missing-index" | "filtered-scan" | "unindexed-fk" | "plan-insights")
                && !first.contains(&f.id)
            {
                f.inactive = true;
            }
        }
        out.push(tile(&dashboard(&r), TileKey::Indexes));
        // One index to clean up and no query history.
        if let Some(cleanup) = advice.cleanups.first() {
            let mut s = base.clone();
            s.workload = None;
            let keep = cleanup.index.clone();
            let others: Vec<String> = advice.cleanups.iter().skip(1).map(|c| c.index.clone()).collect();
            s.indexes.retain(|i| i.name == keep || !others.contains(&i.name));
            out.push(said(s, TileKey::Indexes));
        }
    }
    // Plans: every one historical, none captured with and without a reason, one plan only.
    for name in ["mssql-demo.report.json", "pg-demo.report.json", "mysql-demo.report.json"] {
        let base = snapshot(name);
        let mut s = base.clone();
        for q in s.workload.as_mut().unwrap().queries.iter_mut().filter(|q| q.plan.is_some()) {
            q.executions = 1.0;
        }
        out.push(said(s, TileKey::Plans));
        let mut s = base.clone();
        for q in s.workload.as_mut().unwrap().queries.iter_mut() {
            q.plan = None;
        }
        out.push(said(s.clone(), TileKey::Plans));
        s.server.missing.push(culprit_core::MissingCapability {
            capability: culprit_core::Capability::QueryPlans,
            reason: "The login can't read plans.".into(),
            fix: None,
        });
        out.push(said(s, TileKey::Plans));
        let with_plans: Vec<String> =
            base.workload.as_ref().unwrap().queries.iter().filter(|q| q.plan.is_some()).map(|q| q.id.clone()).collect();
        for keep in with_plans.iter().take(3) {
            let mut s = base.clone();
            for q in s.workload.as_mut().unwrap().queries.iter_mut().filter(|q| &q.id != keep) {
                q.plan = None;
            }
            out.push(said(s, TileKey::Plans));
        }
        for n in [2, 3, 4] {
            let mut s = base.clone();
            for q in s.workload.as_mut().unwrap().queries.iter_mut().filter(|q| !with_plans[..n].contains(&q.id)) {
                q.plan = None;
            }
            out.push(said(s, TileKey::Plans));
        }
    }
    same(&out.join("\n"), INDEXES_PLANS);
}

const INDEXES_PLANS: &str = r#"Indexes Info Indexes: One new index is worth a look; 2 indexes can go (27.1 MB).
  detail: Index usage counters have counted for 1 h 7 min; an index has to go unread for a week before it's listed to clean up.
  metric To add: 1
  metric To clean up: 2
  metric Space back: 27.1 MB
  metric Tables to look at: 0
  item ticket_events (ticket_id): -
  item ix_orders_status on orders: 26.5 MB
  item ix_products_sku on products: 648 KB
Indexes Info Indexes: One new index would help queries that take 6% of database time; 2 indexes can go (27.1 MB).
  detail: Index usage counters have counted for 1 h 7 min; an index has to go unread for a week before it's listed to clean up.
  metric To add: 1
  metric To clean up: 2
  metric Space back: 27.1 MB
  metric Tables to look at: 0
  item order_items (order_id): 6%
  item ix_orders_status on orders: 26.5 MB
  item ix_products_sku on products: 648 KB
Indexes Info Indexes: One new index is worth a look; 1 index can go (26.5 MB).
  detail: Index usage counters have counted for 1 h 7 min; an index has to go unread for a week before it's listed to clean up.
  metric To add: 1
  metric To clean up: 1
  metric Space back: 26.5 MB
  metric Tables to look at: 0
  item ticket_events (ticket_id): -
  item ix_orders_status on orders: 26.5 MB
Indexes Info Indexes: 3 new indexes are worth a look; 2 indexes can go (13.6 MB).
  detail: PostgreSQL doesn't record missing-index requests. Candidates come from captured plans that read whole tables, foreign keys, and the table scan counters.
  metric To add: 3
  metric To clean up: 2
  metric Space back: 13.6 MB
  metric Tables to look at: 2
  item orders (customer_id): -
  item order_items (product_id): -
  item order_items (order_id): -
  item orders_status_idx on orders: 13.3 MB
  item products_sku_idx on products: 328 KB
Indexes Watch Indexes: One new index would help queries that take 23% of database time; 2 indexes can go (13.6 MB).
  detail: PostgreSQL doesn't record missing-index requests. Candidates come from captured plans that read whole tables, foreign keys, and the table scan counters.
  metric To add: 1
  metric To clean up: 2
  metric Space back: 13.6 MB
  metric Tables to look at: 0
  item orders (customer_id): 23%
  item orders_status_idx on orders: 13.3 MB
  item products_sku_idx on products: 328 KB
Indexes Info Indexes: 3 new indexes are worth a look; 1 index can go (13.3 MB).
  detail: PostgreSQL doesn't record missing-index requests. Candidates come from captured plans that read whole tables, foreign keys, and the table scan counters.
  metric To add: 3
  metric To clean up: 1
  metric Space back: 13.3 MB
  metric Tables to look at: 2
  item orders (customer_id): -
  item order_items (product_id): -
  item order_items (order_id): -
  item orders_status_idx on orders: 13.3 MB
Indexes Info Indexes: No query history to suggest new indexes from; 2 existing indexes can go (10.9 MB).
  detail: MySQL doesn't record missing-index requests. Candidates come from captured plans, foreign keys, and rows read without an index (performance_schema).
  metric To add: 0
  metric To clean up: 2
  metric Space back: 10.9 MB
  metric Tables to look at: 1
  item ix_orders_status on orders: 10.5 MB
  item ix_products_sku on products: 432 KB
Indexes Info Indexes: No new indexes needed; 2 existing indexes can go (10.9 MB).
  detail: MySQL doesn't record missing-index requests. Candidates come from captured plans, foreign keys, and rows read without an index (performance_schema).
  metric To add: 0
  metric To clean up: 2
  metric Space back: 10.9 MB
  metric Tables to look at: 1
  item ix_orders_status on orders: 10.5 MB
  item ix_products_sku on products: 432 KB
Indexes Info Indexes: No query history to suggest new indexes from; 1 existing index can go (10.5 MB).
  detail: MySQL doesn't record missing-index requests. Candidates come from captured plans, foreign keys, and rows read without an index (performance_schema).
  metric To add: 0
  metric To clean up: 1
  metric Space back: 10.5 MB
  metric Tables to look at: 1
  item ix_orders_status on orders: 10.5 MB
Indexes Info Indexes: 2 new indexes are worth a look; 1 index can go (3.5 MB).
  detail: SQLite keeps no history of the statements it ran, so candidates come from foreign keys only. With the application's query log on the connection, the indexes SQLite builds for itself on every run join them.
  metric To add: 2
  metric To clean up: 1
  metric Space back: 3.5 MB
  metric Tables to look at: 0
  item order_items (product_id): -
  item orders (customer_id): -
  item ix_orders_status_again on orders: 3.5 MB
Indexes Watch Indexes: One new index would help queries that take 99% of database time; 1 index can go (3.5 MB).
  detail: SQLite doesn't record missing-index requests. Candidates come from the indexes it builds for itself on every run of the logged statements, and foreign keys.
  metric To add: 1
  metric To clean up: 1
  metric Space back: 3.5 MB
  metric Tables to look at: 0
  item orders (customer_id): 99%
  item ix_orders_status_again on orders: 3.5 MB
Indexes Info Indexes: 2 new indexes are worth a look; 1 index can go (3.5 MB).
  detail: SQLite keeps no history of the statements it ran, so candidates come from foreign keys only. With the application's query log on the connection, the indexes SQLite builds for itself on every run join them.
  metric To add: 2
  metric To clean up: 1
  metric Space back: 3.5 MB
  metric Tables to look at: 0
  item order_items (product_id): -
  item orders (customer_id): -
  item ix_orders_status_again on orders: 3.5 MB
Plans Info Plans: 13 application queries have plans, all historical or once-only; there is no current plan assessment.
  detail: Historical plans remain available under Queries; recheck their structural observations if the workload returns.
  metric Current plans: 0
  metric All queries with plans: 13
Plans Unavailable Plans: No plans were captured in this scan.
  detail: Open a query to plan it on demand, or paste a plan into Plans.
Plans Unavailable Plans: No plans were captured: the login can't read plans.
  detail: Open a query to plan it on demand, or paste a plan into Plans.
Plans Watch Plans: One of the 1 current query plan has something to fix: an implicit conversion (7% of database time).
  detail: 1 current application queries have plans; 1 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
  metric Current plans: 1
  metric With problems: 1
  item An implicit conversion: 1 plan
Plans Watch Plans: One of the 1 current query plan has something to fix: a missing index (2% of database time).
  detail: 1 current application queries have plans; 1 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
  metric Current plans: 1
  metric With problems: 1
  item A missing index: 1 plan
Plans Good Plans: None of the 1 current query plan shows a known problem.
  detail: 1 current application queries have plans; 1 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
  metric Current plans: 1
  metric With problems: 0
Plans Watch Plans: 2 of 2 current query plans have something to fix; the costliest is an implicit conversion (7% of database time).
  detail: 2 current application queries have plans; 2 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
  metric Current plans: 2
  metric With problems: 2
  item An implicit conversion: 1 plan
  item A missing index: 1 plan
Plans Watch Plans: 2 of 3 current query plans have something to fix; the costliest is an implicit conversion (7% of database time).
  detail: 3 current application queries have plans; 3 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
  metric Current plans: 3
  metric With problems: 2
  item An implicit conversion: 1 plan
  item A missing index: 1 plan
Plans Watch Plans: 3 of 4 current query plans have something to fix; the costliest is an implicit conversion (7% of database time).
  detail: 4 current application queries have plans; 4 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
  metric Current plans: 4
  metric With problems: 3
  item An implicit conversion: 1 plan
  item A missing index: 2 plans
Plans Info Plans: 12 application queries have plans, all historical or once-only; there is no current plan assessment.
  detail: Historical plans remain available under Queries; recheck their structural observations if the workload returns.
  metric Current plans: 0
  metric All queries with plans: 12
Plans Unavailable Plans: No plans were captured in this scan.
  detail: Open a query to plan it on demand, or paste a plan into Plans.
Plans Unavailable Plans: No plans were captured: the login can't read plans.
  detail: Open a query to plan it on demand, or paste a plan into Plans.
Plans Good Plans: None of the 1 current query plan shows a known problem.
  detail: 1 current application queries have plans; 1 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. This generic plan was obtained without the application's parameter values. Its estimates and connection settings may differ from an application's run, and partitions listed here may be skipped at runtime. It does not identify the plan used by historical executions. Actual rows and timing require a measured plan with representative values.
  metric Current plans: 1
  metric With problems: 0
Plans Watch Plans: One of the 1 current query plan has something to fix: a scan reading far more rows than it keeps (23% of database time).
  detail: 1 current application queries have plans; 1 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. This generic plan was obtained without the application's parameter values. Its estimates and connection settings may differ from an application's run, and partitions listed here may be skipped at runtime. It does not identify the plan used by historical executions. Actual rows and timing require a measured plan with representative values.
  metric Current plans: 1
  metric With problems: 1
  item A scan reading far more rows than it keeps: 1 plan
Plans Info Plans: 1 application queries have plans, all historical or once-only; there is no current plan assessment.
  detail: Historical plans remain available under Queries; recheck their structural observations if the workload returns.
  metric Current plans: 0
  metric All queries with plans: 1
Plans Watch Plans: One of the 2 current query plans has something to fix: a scan reading far more rows than it keeps (23% of database time).
  detail: 2 current application queries have plans; 2 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. This generic plan was obtained without the application's parameter values. Its estimates and connection settings may differ from an application's run, and partitions listed here may be skipped at runtime. It does not identify the plan used by historical executions. Actual rows and timing require a measured plan with representative values.
  metric Current plans: 2
  metric With problems: 1
  item A scan reading far more rows than it keeps: 1 plan
Plans Watch Plans: One of the 2 current query plans has something to fix: a scan reading far more rows than it keeps (23% of database time).
  detail: 2 current application queries have plans; 3 application queries have plans in all. 1 historical or once-only queries are excluded from the current assessment. This generic plan was obtained without the application's parameter values. Its estimates and connection settings may differ from an application's run, and partitions listed here may be skipped at runtime. It does not identify the plan used by historical executions. Actual rows and timing require a measured plan with representative values.
  metric Current plans: 2
  metric With problems: 1
  item A scan reading far more rows than it keeps: 1 plan
Plans Watch Plans: One of the 2 current query plans has something to fix: a scan reading far more rows than it keeps (23% of database time).
  detail: 2 current application queries have plans; 4 application queries have plans in all. 2 historical or once-only queries are excluded from the current assessment. This generic plan was obtained without the application's parameter values. Its estimates and connection settings may differ from an application's run, and partitions listed here may be skipped at runtime. It does not identify the plan used by historical executions. Actual rows and timing require a measured plan with representative values.
  metric Current plans: 2
  metric With problems: 1
  item A scan reading far more rows than it keeps: 1 plan
Plans Info Plans: 8 application queries have plans, all historical or once-only; there is no current plan assessment.
  detail: Historical plans remain available under Queries; recheck their structural observations if the workload returns.
  metric Current plans: 0
  metric All queries with plans: 8
Plans Unavailable Plans: No plans were captured in this scan.
  detail: Open a query to plan it on demand, or paste a plan into Plans.
Plans Unavailable Plans: No plans were captured: the login can't read plans.
  detail: Open a query to plan it on demand, or paste a plan into Plans.
Plans Info Plans: No urgent plan change is established in this workload; 1 current query plan retains structural observations to review as the data or workload changes.
  detail: 1 current application queries have plans; 1 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
  metric Current plans: 1
  metric With problems: 0
Plans Good Plans: None of the 1 current query plan shows a known problem.
  detail: 1 current application queries have plans; 1 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
  metric Current plans: 1
  metric With problems: 0
Plans Good Plans: None of the 1 current query plan shows a known problem.
  detail: 1 current application queries have plans; 1 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
  metric Current plans: 1
  metric With problems: 0
Plans Info Plans: No urgent plan change is established in this workload; 1 current query plan retains structural observations to review as the data or workload changes.
  detail: 2 current application queries have plans; 2 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
  metric Current plans: 2
  metric With problems: 0
Plans Info Plans: No urgent plan change is established in this workload; 1 current query plan retains structural observations to review as the data or workload changes.
  detail: 3 current application queries have plans; 3 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
  metric Current plans: 3
  metric With problems: 0
Plans Info Plans: No urgent plan change is established in this workload; 1 current query plan retains structural observations to review as the data or workload changes.
  detail: 4 current application queries have plans; 4 application queries have plans in all. 0 historical or once-only queries are excluded from the current assessment. MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
  metric Current plans: 4
  metric With problems: 0"#;

fn session(id: &str, state: &str) -> Session {
    serde_json::from_value(json!({"id": id, "state": state})).unwrap_or_else(|_| Session {
        id: id.into(),
        user: None,
        application: None,
        host: None,
        database: None,
        state: state.into(),
        command: None,
        wait: None,
        wait_ms: None,
        blocked_by: None,
        elapsed_ms: None,
        transaction_ms: None,
        cpu_ms: None,
        reads: None,
        sql: None,
    })
}

fn live(sessions: Vec<Session>, chains: bool) -> LiveState {
    let blocking = chains.then(|| culprit_core::blocking::analyze(Engine::Postgres, &sessions, &[])).flatten();
    LiveState {
        notes: Vec::new(),
        taken_at: "2026-10-11T12:00:00Z".into(),
        sessions,
        waits: vec![],
        session_actions: vec![],
        session_actions_note: None,
        blocking,
    }
}

#[test]
fn sessions_and_the_headline_say_what_they_said() {
    let mut out = Vec::new();
    let board = dashboard(&report("pg-demo.report.json"));
    let mut quick = session("51", "active");
    quick.elapsed_ms = Some(200.0);
    quick.command = Some("SELECT".into());
    let mut slow = session("52", "active");
    (slow.elapsed_ms, slow.sql) = (Some(4_000.0), Some("SELECT * FROM orders WHERE id = $1".into()));
    let mut waiting = session("53", "active");
    (waiting.blocked_by, waiting.wait_ms, waiting.elapsed_ms) = (Some("52".into()), Some(2_000.0), Some(2_000.0));
    let mut other = waiting.clone();
    other.id = "54".into();
    for (sessions, chains) in [
        (vec![quick.clone()], false),
        (vec![quick.clone(), slow.clone()], false),
        (vec![], false),
        (vec![slow.clone(), waiting.clone()], true),
        (vec![slow.clone(), waiting.clone()], false),
        (vec![slow.clone(), waiting.clone(), other.clone()], false),
    ] {
        let now = refresh(board.clone(), Ok(live(sessions, chains)));
        out.push(tile(&now.dashboard, TileKey::Now));
    }
    let failed = refresh(
        board.clone(),
        Err(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "Permission denied for pg_stat_activity",
        )),
    );
    out.push(tile(&failed.dashboard, TileKey::Now));
    out.push(format!("headline: {}", failed.dashboard.headline));
    // Nothing needs a look: every tile has data, one hasn't, two haven't.
    for missing in [0, 1, 2] {
        let mut d = board.clone();
        for (k, t) in d.tiles.iter_mut().enumerate() {
            t.status = if k > 0 && k <= missing {
                culprit_core::TileStatus::Unavailable
            } else {
                culprit_core::TileStatus::Good
            };
        }
        with_now(&mut d, &live(vec![], true));
        out.push(format!("headline: {}", d.headline));
    }
    same(&out.join("\n"), SESSIONS);
}

const SESSIONS: &str = r#"Now Good Right now: 1 session running, none blocked.
  detail: Read Sun 12:00 UTC. 
  metric Running: 1
  metric Identified blocked sessions: 0
  metric Idle in transaction: 0
  metric Long open idle transactions: 0
  metric Sessions: 1
  item Session 51 · SELECT: 200 ms
Now Good Right now: 2 sessions running, none blocked; the longest has run 4.0 s.
  detail: Read Sun 12:00 UTC. 
  metric Running: 2
  metric Identified blocked sessions: 0
  metric Idle in transaction: 0
  metric Long open idle transactions: 0
  metric Sessions: 2
  item Session 52 · a read from orders: 4.0 s
  item Session 51 · SELECT: 200 ms
Now Good Right now: Nothing is running right now.
  detail: Read Sun 12:00 UTC. 
  metric Running: 0
  metric Identified blocked sessions: 0
  metric Idle in transaction: 0
  metric Long open idle transactions: 0
  metric Sessions: 0
Now Watch Right now: Session 52 has been running a SELECT statement for 4.0 s while holding locks; 1 session waits behind it.
  detail: Read Sun 12:00 UTC. 
  metric Running: 2
  metric Identified blocked sessions: 1
  metric Idle in transaction: 0
  metric Long open idle transactions: 0
  metric Sessions: 2
  item Session 52 has been running a SELECT statement for 4.0 s while holding locks; 1 session waits behind it.: 1 waiting
  item Session 53 · active: 2.0 s
Now Watch Right now: 1 session is waiting on another session's lock.
  detail: Read Sun 12:00 UTC. 
  metric Running: 2
  metric Identified blocked sessions: 1
  metric Idle in transaction: 0
  metric Long open idle transactions: 0
  metric Sessions: 2
  item Session 52 · a read from orders: 4.0 s
  item Session 53 · active: 2.0 s
Now Watch Right now: 2 sessions are waiting on another session's lock.
  detail: Read Sun 12:00 UTC. 
  metric Running: 3
  metric Identified blocked sessions: 2
  metric Idle in transaction: 0
  metric Long open idle transactions: 0
  metric Sessions: 3
  item Session 52 · a read from orders: 4.0 s
  item Session 53 · active: 2.0 s
  item Session 54 · active: 2.0 s
Now Unavailable Right now: Couldn't read sessions: permission denied for pg_stat_activity.
  detail: Everything else here comes from the latest scan.
headline: 14 opportunities; the top ones touch 2 min of database time over the 5 hours since statistics were reset (67%).
headline: Nothing needs a look right now.
headline: Nothing needs a look in what this scan can see; one tile has no data here.
headline: Nothing needs a look in what this scan can see; 2 tiles have no data here."#;

#[test]
fn storage_connections_replication_failures_and_statistics_say_what_they_said() {
    let mut out = Vec::new();
    let pg = snapshot("pg-demo.report.json");
    // PostgreSQL's kinds of I/O, timed: WAL flushes, temporary files and tables; then too few to judge.
    let io = |ops: f64| {
        edited(&pg, |v| {
            let files = &mut v["pressure"]["files"];
            files[0]["write_ms"] = json!(ops * 0.5);
            files[0]["writes"] = json!(ops);
            files[1]["reads"] = json!(ops);
            files[1]["read_ms"] = json!(ops * 0.02);
            let mut temp = files[1].clone();
            temp["kind"] = json!("temp");
            temp["name"] = json!("Temporary files");
            temp["read_ms"] = json!(ops * 3.0);
            files.as_array_mut().unwrap().push(temp);
        })
    };
    out.push(said(io(50_000.0), TileKey::Storage));
    out.push(said(io(10.0), TileKey::Storage));
    let mssql = snapshot("mssql-demo.report.json");
    let mut s = edited(&mssql, |v| {
        for f in v["pressure"]["files"].as_array_mut().unwrap() {
            if f["kind"] == json!("log") {
                f["write_operation"] = json!("write");
            }
        }
    });
    out.push(said(s.clone(), TileKey::Storage));
    s.pressure.as_mut().unwrap().files.clear();
    out.push(said(s, TileKey::Storage));
    // New connections a second, and none counted.
    let mysql = snapshot("mysql-demo.report.json");
    let mut s = mysql.clone();
    if let Some(sig) =
        s.pressure.as_mut().unwrap().signals.iter_mut().find(|g| g.key == culprit_core::signal_id::NEW_CONNECTIONS)
    {
        sig.value = 12.5;
    }
    out.push(said(s.clone(), TileKey::Connections));
    s.pressure.as_mut().unwrap().signals.retain(|g| g.key != culprit_core::signal_id::NEW_CONNECTIONS);
    out.push(said(s, TileKey::Connections));
    // Replicas behind by time, by log, by state and in sync; a replication source that couldn't be read.
    let s = edited(&pg, |v| {
        let slot = v["pressure"]["replicas"][0].clone();
        let mut all = Vec::new();
        for (name, role, seconds, bytes, state, condition) in [
            ("standby-a", "replica", json!(42.0), json!(null), json!(null), "reported"),
            ("standby-b", "this-server", json!(null), json!(null), json!("streaming"), "reported"),
            ("standby-c", "replica", json!(null), json!(null), json!(null), "reported"),
            ("analytics_slot", "slot", json!(null), json!(1172682184.0), json!(null), "slot_inactive"),
        ] {
            let mut r = slot.clone();
            (r["name"], r["role"], r["lag_seconds"], r["lag_bytes"], r["state"], r["condition"]) =
                (json!(name), json!(role), seconds, bytes, state, json!(condition));
            all.push(r);
        }
        v["pressure"]["replicas"] = json!(all);
    });
    out.push(said(s, TileKey::Replication));
    let mut s = pg.clone();
    s.pressure.as_mut().unwrap().replicas.clear();
    s.notes.push(culprit_core::CollectorNote {
        collector: culprit_core::source_id::REPLICATION.into(),
        message: "The login can't read pg_stat_replication.".into(),
        table: None,
    });
    out.push(said(s, TileKey::Replication));
    for name in
        ["mssql-demo.report.json", "mysql-demo.report.json", "sqlite-demo.report.json", "yugabyte-demo.report.json"]
    {
        let mut s = snapshot(name);
        if let Some(p) = s.pressure.as_mut() {
            p.replicas.clear();
        }
        s.notes.retain(|n| n.collector != culprit_core::source_id::REPLICATION);
        out.push(said(s, TileKey::Replication));
    }
    // Terminations retained: none, one, a partial read; and a history that couldn't be read.
    let yb = snapshot("yugabyte-demo.report.json");
    for (events, partial) in [(0, false), (1, true), (3, false)] {
        let s = edited(&yb, |v| {
            let h = &mut v["operations"]["termination_history"];
            let list: Vec<_> = h["events"].as_array().unwrap().iter().take(events).cloned().collect();
            h["events"] = json!(list);
            h["partial"] = json!(partial);
        });
        out.push(said(s, TileKey::Failures));
    }
    let mut s = edited(&yb, |v| v["operations"]["termination_history"] = json!(null));
    crate::record(
        &mut s,
        culprit_core::source_id::DOCDB_TERMINATIONS,
        culprit_core::SourceState::Unavailable,
        Some("The login can't read yb_terminated_queries."),
    );
    out.push(said(s, TileKey::Failures));
    // Statements counted as failed: none, a few, all, and some of each.
    for failed in [&[0.0, 0.0][..], &[2.0, 0.0], &[40.0, 7.0]] {
        let mut s = mysql.clone();
        for (q, n) in s.workload.as_mut().unwrap().queries.iter_mut().zip(failed) {
            let costs = q.costs.get_or_insert_with(Default::default);
            (costs.failed, costs.aborted) = (Some(*n), Some(0.0));
        }
        for q in s.workload.as_mut().unwrap().queries.iter_mut().skip(failed.len()) {
            if let Some(costs) = q.costs.as_mut() {
                (costs.failed, costs.aborted) = (Some(0.0), Some(0.0));
            }
        }
        let mut r = report("mysql-demo.report.json");
        r.snapshot = s;
        r.findings.retain(|f| f.check != "failing-query");
        out.push(tile(&dashboard(&r), TileKey::Failures));
    }
    let slow_log = edited(&mysql, |v| {
        v["workload"]["kind"] = json!("slow-log");
        for q in v["workload"]["queries"].as_array_mut().unwrap() {
            if q["costs"].is_object() {
                q["costs"]["failed"] = json!(null);
                q["costs"]["aborted"] = json!(null);
            }
        }
    });
    out.push(as_saved("mysql-demo.report.json", slow_log, TileKey::Failures));
    for failed in [&[0.0, 0.0][..], &[2.0, 0.0], &[40.0, 7.0]] {
        let mut s = mysql.clone();
        let w = s.workload.as_mut().unwrap();
        for (q, n) in w.queries.iter_mut().zip(failed) {
            let costs = q.costs.get_or_insert_with(Default::default);
            costs.failed = Some(*n);
            costs.aborted = Some(0.0);
            if *n == 40.0 {
                q.executions = 40.0;
            }
        }
        out.push(said(s, TileKey::Failures));
    }
    // Errors from an application log, and a log that only times statements.
    let sqlite = snapshot("sqlite-demo.report.json");
    let mut s = sqlite.clone();
    s.operations.log_failures.clear();
    out.push(said(s.clone(), TileKey::Failures));
    s.operations.log_failures.push(culprit_core::operations::LoggedFailure {
        query_id: "q1".into(),
        statement: "UPDATE orders SET status = ? WHERE id = ?".into(),
        code: "SQLITE_BUSY".into(),
        count: 1234.0,
        first_at: None,
        last_at: None,
    });
    out.push(as_saved("sqlite-demo.report.json", s, TileKey::Failures));
    // Query Store nearly full; a log that covers minutes; locked checks.
    let mut s = mssql.clone();
    for (name, value) in [
        ("query_store_options.current_storage_size_mb", "950"),
        ("query_store_options.max_storage_size_mb", "1000"),
        ("query_store_options.size_based_cleanup_mode_desc", "OFF"),
    ] {
        s.settings.retain(|x| x.name != name);
        s.settings.push(culprit_core::Setting {
            name: name.into(),
            value: value.into(),
            default_value: None,
            scope: "database".into(),
        });
    }
    out.push(as_saved("mssql-demo.report.json", s, TileKey::Statistics));
    let mut s = sqlite.clone();
    let w = s.workload.as_mut().unwrap();
    crate::last_hours(w, 0.5);
    out.push(said(s, TileKey::Statistics));
    for name in ["mssql-demo-staging.report.json", "sqlite-demo.report.json", "yugabyte-demo.report.json"] {
        out.push(tile(&dashboard(&report(name)), TileKey::Statistics));
    }
    same(&out.join("\n"), SIGNALS);
}

const SIGNALS: &str = r#"Storage Info Storage: Recorded storage averages: reads average 3 ms, log flushes 500 µs.
  detail: WAL: whole server, over an unknown statistics period; tables and indexes: whole server, over an unknown statistics period; temporary files: whole server, over an unknown statistics period
  metric Avg read: 3 ms
  metric Avg log flush: 500 µs
  item temporary files (whole server): 3 ms per read
  item WAL (whole server): 500 µs per flush
  item tables and indexes (whole server): 20 µs per read
Storage Info Storage: Too little I/O since startup to judge storage speed.
  detail: WAL: whole server, over an unknown statistics period; tables and indexes: whole server, over an unknown statistics period; temporary files: whole server, over an unknown statistics period
Storage Info Storage: Recorded storage averages: reads average 40 µs, log writes 632 µs.
  detail: culprit_demo log: this database, over the recorded history; culprit_demo: this database, over the recorded history; tempdb log: tempdb, over the recorded history; tempdb: tempdb, over the recorded history; tempdb: tempdb, over the recorded history; tempdb: tempdb, over the recorded history; tempdb: tempdb, over the recorded history; tempdb: tempdb, over the recorded history; tempdb: tempdb, over the recorded history; tempdb: tempdb, over the recorded history; tempdb: tempdb, over the recorded history
  metric Avg read: 40 µs
  metric Avg log write: 632 µs
  item culprit_demo log (this database): 632 µs per write
  item culprit_demo (this database): 40 µs per read
Storage Unavailable Storage: This scan has no storage timings.
Connections Info Connections: The whole server recorded 12.5 new connections a second on average.
  metric New per second: 12.5
  item Connections in use (whole server): 4.64%
Connections Info Connections: This scan has no count of new connections; the ones in use are below.
  item Connections in use (whole server): 4.64%
Replication Watch Replication: The replica standby-a is 42.0 s behind.
  item standby-a (replica): 42.0 s behind
  item analytics_slot (slot): 1.1 GB of log
  item standby-b (this server): streaming
  item standby-c (replica): in sync
Replication Unavailable Replication: Couldn't read PostgreSQL's replication state.
  detail: The login can't read pg_stat_replication.
Replication Good Replication: No availability group or geo-replica: nothing can fall behind.
Replication Good Replication: MySQL isn't a replica here and reports none: nothing can fall behind.
Replication Good Replication: A SQLite file has no replicas: nothing can fall behind.
Replication Unavailable Replication: Raft replica lag was not measured; an empty PostgreSQL replication view cannot establish DocDB replication health.
Failures Info Recorded terminations: No unexpected termination records were retained in this read; ordinary SQL errors are not counted here.
  detail: yb_terminated_queries (node 3d2d9e10555b4ae2a683a8402505e416) · 2026-10-17T06:52:49.190547Z to 2026-10-24T06:52:49.190547Z · Complete monitoring privilege for this database. The evictable buffer is independent of statement statistics and does not include all SQL errors, cancellations or deadlocks.
  metric Retained records: 0
Failures Info Recorded terminations: 1 unexpected termination record remains in the native buffer; this is not a failure rate.
  detail: yb_terminated_queries (node 3d2d9e10555b4ae2a683a8402505e416) · 2026-10-17T06:52:49.190547Z to 2026-10-24T06:52:49.190547Z · Complete monitoring privilege for this database. Partial read. The evictable buffer is independent of statement statistics and does not include all SQL errors, cancellations or deadlocks.
  metric Retained records: 1
  item Temporary-file limit exceeded: 2026-10-24T06:47:02.747149Z
Failures Info Recorded terminations: 3 unexpected termination records remain in the native buffer; this is not a failure rate.
  detail: yb_terminated_queries (node 3d2d9e10555b4ae2a683a8402505e416) · 2026-10-17T06:52:49.190547Z to 2026-10-24T06:52:49.190547Z · Complete monitoring privilege for this database. The evictable buffer is independent of statement statistics and does not include all SQL errors, cancellations or deadlocks.
  metric Retained records: 3
  item Temporary-file limit exceeded: 2026-10-24T06:47:02.747149Z
  item Temporary-file limit exceeded: 2026-10-24T06:47:02.326536Z
  item Temporary-file limit exceeded: 2026-10-24T06:47:02.325410Z
Failures Unavailable Failing queries: Native termination history was unavailable.
  detail: The login can't read yb_terminated_queries.
Failures Good Failing queries: No statement failed or timed out over the 2 minutes since statistics were reset.
  metric Statements: 0
  metric Unfinished runs: 0
Failures Info Failing queries: 2 runs didn't finish over the 2 minutes since statistics were reset; the recorded counts do not establish a current error-rate problem.
  metric Statements: 1
  metric Unfinished runs: 2
  item a read from orders: 2 of 20 runs
Failures Info Failing queries: 47 runs didn't finish over the 2 minutes since statistics were reset; the recorded counts do not establish a current error-rate problem.
  metric Statements: 2
  metric Unfinished runs: 47
  item a read from orders: every run
  item a read from orders: 12% of runs
Failures Unavailable Failing queries: The slow query log keeps runs that finished; MySQL counts errors per statement only in performance_schema.
Failures Bad Failing queries: An update of orders fails 28% of the time (1205 lock wait timeout).
  metric Statements: 2
  metric Unfinished runs: 13
  item an update of orders: 28% of runs
  item an update of orders: 1 of 8 runs
Failures Bad Failing queries: An update of orders fails 28% of the time (1205 lock wait timeout).
  metric Statements: 3
  metric Unfinished runs: 15
  item an update of orders: 28% of runs
  item a read from orders: 2 of 20 runs
  item an update of orders: 1 of 8 runs
Failures Bad Failing queries: An update of orders fails 28% of the time (1205 lock wait timeout).
  metric Statements: 4
  metric Unfinished runs: 60
  item a read from orders: every run
  item an update of orders: 28% of runs
  item a read from orders: 12% of runs
  item an update of orders: 1 of 8 runs
Failures Unavailable Failing queries: The Rails log records how long statements took, not whether they failed.
Failures Info Failing queries: 1.2K SQL error records in the supplied application log.
  detail: Counts are logged records, including possible retries and duplicates. Dates cover only records with a recorded time zone; missing durations are excluded from performance totals.
  metric Error log records: 1.2K
Statistics Watch Query statistics: Query Store is 95% full; it stops recording when it fills.
  detail: 1 check is locked: Repeated statements within logged requests needs Application query log.
  metric Queries: 16
  metric All queries with plans: 13
  metric Covers: 5 hours
  metric Locked checks: 1
  item Query Store records every statement, including one-off ones: -
Statistics Watch Query statistics: Only the 30 minutes the Rails log covers, so the numbers are thin: a longer log tells more.
  metric Queries: 7
  metric All queries with plans: 5
  metric Covers: 30 minutes
  metric Locked checks: 0
Statistics Bad Query statistics: No query history: Query Store is OFF on this database.
  detail: Until they're on, Culprit checks tables, indexes and settings only: no query list, plans or trends.
  metric Locked checks: 6
  item Query Store: off
Statistics Good Query statistics: Over the 90 minutes the Rails log covers: 7 queries, 5 with plans.
  metric Queries: 7
  metric All queries with plans: 5
  metric Covers: 90 minutes
  metric Locked checks: 0
Statistics Watch Query statistics: YSQL pg_stat_statements (node 3d2d9e10555b4ae2a683a8402505e416) covers only the 1 minute since statistics were reset, so the numbers are thin until it has seen more.
  detail: 3 checks are locked: Unused indexes, Overlapping indexes and Repeated statements within logged requests need Application query log and Index usage statistics.
  metric Queries: 5
  metric All queries with plans: 5
  metric Covers: 1 minute
  metric Locked checks: 3"#;

#[test]
fn top_queries_and_waits_say_what_they_said() {
    let mut out = Vec::new();
    // The top queries: one, three and five making up the time; ones that stopped or ran once.
    let mssql = snapshot("mssql-demo.report.json");
    for spread in [
        &[0.9, 0.05, 0.05][..],
        &[0.25, 0.25, 0.2, 0.15, 0.15],
        &[0.2, 0.2, 0.2, 0.2, 0.2, 0.1],
        &[0.5, 0.5],
        &[0.2, 0.2],
        &[0.25],
    ] {
        let mut s = mssql.clone();
        let w = s.workload.as_mut().unwrap();
        let total = w.total_time_ms;
        for (k, q) in w.queries.iter_mut().filter(|q| q.total_time_ms > 0.0).enumerate() {
            q.total_time_ms = spread.get(k).copied().unwrap_or(0.0) * total;
        }
        out.push(said(s, TileKey::TopQueries));
    }
    // Four statements over two days: one ran once, one stopped a day and a half before the end; then all stopped.
    let mut s = mssql.clone();
    let w = s.workload.as_mut().unwrap();
    crate::last_hours(w, 48.0);
    let end = culprit_core::format::epoch_seconds(w.window_end.as_deref().unwrap()).unwrap();
    let before_end = culprit_core::format::iso_from_epoch(end - 36.0 * 3600.0);
    let total = w.total_time_ms;
    let mut k = 0;
    for q in w.queries.iter_mut() {
        q.total_time_ms = if q.total_time_ms > 0.0 && k < 4 { total * 0.2 } else { 0.0 };
        if q.total_time_ms > 0.0 {
            match k {
                0 => q.executions = 1.0,
                1 => q.last_seen = Some(before_end.clone()),
                _ => {}
            }
            k += 1;
        }
    }
    out.push(as_saved("mssql-demo.report.json", s.clone(), TileKey::TopQueries));
    for q in s.workload.as_mut().unwrap().queries.iter_mut() {
        q.last_seen = Some(before_end.clone());
        q.executions = q.executions.max(100.0);
    }
    out.push(as_saved("mssql-demo.report.json", s, TileKey::TopQueries));
    let mut s = mssql.clone();
    s.workload.as_mut().unwrap().queries.clear();
    out.push(said(s, TileKey::TopQueries));
    // Retained activity with nothing retained, or a partial read.
    let yb = snapshot("yugabyte-demo.report.json");
    for (events, partial) in [(0, false), (257, true)] {
        let s = edited(&yb, |v| {
            let a = &mut v["operations"]["activity_history"];
            let list: Vec<_> = a["events"].as_array().unwrap().iter().take(events).cloned().collect();
            a["events"] = json!(list);
            a["period"]["partial"] = json!(partial);
            if events == 0 {
                a["period"]["first_sample"] = json!(null);
                a["period"]["last_sample"] = json!(null);
            }
        });
        out.push(said(s, TileKey::Waits));
    }
    // Waits the server doesn't break down by kind, or only partly; SQL Server without wait statistics.
    let mut s = mssql.clone();
    for w in s.waits.iter_mut() {
        w.category = "other".into();
    }
    out.push(said(s.clone(), TileKey::Waits));
    s.waits.clear();
    out.push(said(s.clone(), TileKey::Waits));
    s.server.missing.push(culprit_core::MissingCapability {
        capability: culprit_core::Capability::WaitStats,
        reason: "The login lacks VIEW SERVER STATE".into(),
        fix: Some("GRANT VIEW SERVER STATE TO culprit;".into()),
    });
    out.push(said(s, TileKey::Waits));
    for name in ["pg-demo.report.json", "mysql-demo.report.json", "mariadb-demo.report.json"] {
        let mut s = snapshot(name);
        s.waits.clear();
        if let Some(w) = s.workload.as_mut() {
            for q in w.queries.iter_mut() {
                q.waits.clear();
            }
        }
        out.push(said(s, TileKey::Waits));
    }
    same(&out.join("\n"), QUERIES_WAITS);
}

const QUERIES_WAITS: &str = r#"TopQueries Info Top queries: One query is 90% of database time: a read from customers.
  detail: 16 queries tracked over the last 5 hours.
  metric Top query: 90%
  metric Top five: 100%
  metric Queries: 16
  item a read from customers: 90%
  item a read from order_items: 5%
  item a read from support_tickets: 5%
TopQueries Info Top queries: Three queries make up 70% of database time.
  detail: 16 queries tracked over the last 5 hours.
  metric Top query: 25%
  metric Top five: 100%
  metric Queries: 16
  item a read from customers: 25%
  item a read from order_items: 25%
  item a read from support_tickets: 20%
  item a read from orders: 15%
  item a read from products: 15%
TopQueries Info Top queries: Three queries make up 60% of database time.
  detail: 16 queries tracked over the last 5 hours.
  metric Top query: 20%
  metric Top five: 100%
  metric Queries: 16
  item a read from customers: 20%
  item a read from order_items: 20%
  item a read from support_tickets: 20%
  item a read from orders: 20%
  item a read from products: 20%
TopQueries Info Top queries: One query is 50% of database time: a read from customers.
  detail: 16 queries tracked over the last 5 hours.
  metric Top query: 50%
  metric Top five: 100%
  metric Queries: 16
  item a read from customers: 50%
  item a read from order_items: 50%
TopQueries Info Top queries: 2 queries are 40% of database time.
  detail: 16 queries tracked over the last 5 hours.
  metric Top query: 20%
  metric Top five: 40%
  metric Queries: 16
  item a read from customers: 20%
  item a read from order_items: 20%
TopQueries Info Top queries: 1 query is 25% of database time.
  detail: 16 queries tracked over the last 5 hours.
  metric Top query: 25%
  metric Top five: 25%
  metric Queries: 16
  item a read from customers: 25%
TopQueries Info Top queries: Three queries make up 60% of database time.
  detail: 16 queries tracked over the last 2 days. 1 of these stopped running before the window ended: a deploy may have changed it.
  metric Top query: 20%
  metric Top five: 80%
  metric Queries: 16
  item a read from support_tickets: 20%
  item a read from orders: 20%
  item a read from customers (ran once): 20%
  item a read from order_items (stopped running): 20%
TopQueries Info Top queries: Three queries make up 60% of database time.
  detail: 16 queries tracked over the last 2 days. All of these stopped running before the window ended: a deploy may have changed them.
  metric Top query: 20%
  metric Top five: 80%
  metric Queries: 16
  item a read from customers (stopped running): 20%
  item a read from order_items (stopped running): 20%
  item a read from support_tickets (stopped running): 20%
  item a read from orders (stopped running): 20%
TopQueries Info Top queries: No application queries ran over the last 5 hours.
  detail: Query Store recorded 16 statements in all.
Waits Info Recorded activity: Recorded activity has no retained observations in this period.
  detail: yb_active_session_history (node 3d2d9e10555b4ae2a683a8402505e416) · observations unavailable to unavailable. Sampling describes active tasks, not elapsed query time or CPU utilization.
  metric Retained observations: 0
  metric Sample weight: 0
Waits Info Recorded activity: Distributed RPCs account for 55% of retained active-task sample weight.
  detail: yb_active_session_history (node 3d2d9e10555b4ae2a683a8402505e416) · observations 2026-10-23T21:02:32.797443Z to 2026-10-24T06:47:07.261143Z. This read was partial. Sampling describes active tasks, not elapsed query time or CPU utilization.
  metric Retained observations: 720
  metric Sample weight: 720
  item Distributed RPCs: 55% of sample weight
  item CPU: 17% of sample weight
  item Everything else: 14% of sample weight
  item Log writes: 9% of sample weight
  item Locks: 4% of sample weight
Waits Info Where time goes: SQL Server doesn't break this waiting down by kind.
  detail: An update of wide accounts for <1% of it.
  metric Waiting: 2 min
  item Everything else: 100%
Waits Unavailable Where time goes: This scan recorded no waits.
  detail: Sample what it waits on right now instead.
Waits Unavailable Where time goes: Waits aren't readable here: the login lacks VIEW SERVER STATE.
  detail: GRANT VIEW SERVER STATE TO culprit;
Waits Unavailable Where time goes: PostgreSQL keeps no history of what sessions wait on.
  detail: Sample what it waits on right now to see where time goes.
Waits Unavailable Where time goes: MySQL recorded no waits in this scan.
  detail: Sample what it waits on right now instead.
Waits Unavailable Where time goes: MariaDB recorded no waits in this scan.
  detail: Sample what it waits on right now instead."#;

/// One tile that names its engine for every engine, with what that engine lacks.
#[test]
fn every_engine_says_what_it_lacks() {
    let mut out = Vec::new();
    for engine in Engine::ALL {
        let mut s = snapshot("pg-demo.report.json");
        s.server.engine = engine;
        s.server.version = "13.4".into();
        s.pressure = None;
        s.deadlocks = None;
        s.notes.clear();
        let d = dashboard(&analyze(s));
        for key in [TileKey::Now, TileKey::Health, TileKey::Deadlocks, TileKey::Connections, TileKey::Replication] {
            out.push(tile(&d, key));
        }
    }
    same(&out.join("\n"), ENGINES);
}

const ENGINES: &str = r#"Now Info Right now: Sessions weren't read for this dashboard.
  detail: The app reads them every few seconds; `culprit live` reads them once.
Health Unavailable Health: This scan recorded no health counters.
  detail: Scan again to read compiles, memory, cache and connection counters.
Deadlocks Unavailable Deadlocks: This scan didn't read deadlocks.
  detail: Scan again to read the last day of deadlocks.
Connections Unavailable Connections: This scan has no connection counters.
Replication Good Replication: No availability group or geo-replica: nothing can fall behind.
Now Info Right now: Sessions weren't read for this dashboard.
  detail: The app reads them every few seconds; `culprit live` reads them once.
Health Unavailable Health: This scan recorded no health counters.
  detail: Scan again to read compiles, memory, cache and connection counters.
Deadlocks Unavailable Deadlocks: This scan didn't read deadlocks.
  detail: Scan again to read the last day of deadlocks.
Connections Unavailable Connections: PostgreSQL counts sessions from version 14; this server doesn't.
Replication Good Replication: No replicas or replication slots: nothing can fall behind or hold WAL back.
Now Info Right now: Sessions weren't read for this dashboard.
  detail: The app reads them every few seconds; `culprit live` reads them once.
Health Unavailable Health: This scan recorded no health counters.
  detail: Scan again to read compiles, memory, cache and connection counters.
Deadlocks Unavailable Deadlocks: Deadlock history is unavailable.
  detail: DocDB's distributed deadlock detection is not PostgreSQL's heap lock accounting. A PostgreSQL deadlock counter does not establish this database's distributed deadlocks. Correlate YSQL errors and the YB-TServer logs for the affected transaction.
Connections Unavailable Connections: This scan has no connection counters.
Replication Unavailable Replication: Raft replica lag was not measured; an empty PostgreSQL replication view cannot establish DocDB replication health.
Now Info Right now: Sessions weren't read for this dashboard.
  detail: The app reads them every few seconds; `culprit live` reads them once.
Health Unavailable Health: This scan recorded no health counters.
  detail: Scan again to read compiles, memory, cache and connection counters.
Deadlocks Unavailable Deadlocks: This scan didn't read deadlocks.
  detail: Scan again to read the last day of deadlocks.
Connections Unavailable Connections: This scan has no connection counters.
Replication Good Replication: MySQL isn't a replica here and reports none: nothing can fall behind.
Now Unavailable Right now: SQLite has no server, so there are no sessions to show.
  detail: A writer that has to wait for the lock gets SQLITE_BUSY in the application: its logs say when.
Health Unavailable Health: SQLite has no server counters.
  detail: SQLite has no server, so there are no compiles, memory, cache or connection counters to read.
Deadlocks Unavailable Deadlocks: SQLite has no deadlocks to read.
  detail: SQLite records no deadlocks: one writer at a time holds the file.
Connections Unavailable Connections: SQLite has no server, so no connections to count.
Replication Good Replication: A SQLite file has no replicas: nothing can fall behind."#;
