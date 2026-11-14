//! SQL Server counts lock escalations over metadata-cache lifetime and keeps no time for each. Scans compare
//! observed increases require earlier per-index native ownership/reset proof. Digests retain raw context, not that proof.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use culprit_core::{
    AuthKind, DatabaseIdentity, DatabaseLocation, Digest, Engine, Error, ExplainMode, IndexContention, LiveState, Plan,
    Provider, Report, Result, ResultSet, ServerInfo, Severity, Snapshot, SnapshotOptions,
};
use culprit_engine::{NewConnection, Workspace};

const ID: &str = "lock-escalations:shop.orders";

fn temp_home(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("culprit-escalations-{tag}-{}", std::process::id()))
}

/// A controlled single-index native population built on the demo; ordinary native fixture points stay unchanged.
fn demo(escalations: u32) -> Snapshot {
    use culprit_core::telemetry::{NativeIdentity, SharedResourceIdentity, SharedResourceKind};
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mssql-demo.json");
    let mut s: Snapshot = serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("snapshot");
    let mut orders =
        s.indexes.iter().find(|i| i.schema == "shop" && i.table == "orders").expect("an orders index").clone();
    orders.contention = Some(IndexContention {
        source_owner: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Node,
            namespace: NativeIdentity::new("controlled-native-cluster"),
            native: NativeIdentity::new("controlled-native-member"),
        }),
        reset: culprit_core::ResetMarker::Generation("controlled-native-index-generation".into()),
        lock_escalations: f64::from(escalations),
        ..Default::default()
    });
    s.indexes = vec![orders];
    // Server uptime does not establish the metadata counters' epoch.
    let started = chrono::Utc::now() - chrono::Duration::hours(12);
    s.server.started_at = Some(started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    s
}

/// Replays the demo with however many escalations the test has set.
struct Replay(Arc<AtomicU32>);

#[async_trait]
impl Provider for Replay {
    fn engine(&self) -> Engine {
        Engine::Mssql
    }

    async fn probe(&self) -> Result<ServerInfo> {
        Ok(demo(0).server)
    }

    async fn snapshot(&self, _connection: &str, _options: &SnapshotOptions) -> Result<Snapshot> {
        Ok(demo(self.0.load(Ordering::SeqCst)))
    }

    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        Err(Error::invalid("replay has no plans"))
    }

    async fn live(&self) -> Result<LiveState> {
        Err(Error::invalid("replay has no live state"))
    }

    async fn run(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(Error::invalid("replay runs nothing"))
    }

    async fn execute(&self, _: &str, _: usize) -> Result<ResultSet> {
        Err(Error::invalid("replay runs nothing"))
    }
}

fn escalation(report: &Report) -> &culprit_core::Finding {
    report.findings.iter().find(|f| f.id == ID).expect("the escalation finding")
}

fn assert_raw_escalation_context(digest: &Digest, count: u32) {
    let finding = digest.findings.iter().find(|finding| finding.id == ID).expect("raw escalation context");
    assert_eq!(finding.count, Some(f64::from(count)));
    assert_eq!(finding.count_since, None);
    assert_eq!(finding.severity, Severity::Low);
    assert_eq!(finding.actionability, culprit_core::Actionability::Information);
    assert!(digest.top.iter().all(|finding| finding.id != ID));
    assert!(digest.since.as_ref().is_none_or(|since| since.again.iter().all(|finding| finding.id != ID)));
}

#[tokio::test]
async fn each_scan_compares_escalations_with_the_scan_before() {
    let home = temp_home("scans");
    let count = Arc::new(AtomicU32::new(2));
    let replay = count.clone();
    let ws = Workspace::open_in(&home).with_provider(
        Engine::Mssql,
        culprit_engine::ProviderRegistration::new(
            Arc::new(move |_| Ok(Box::new(Replay(replay.clone())) as Box<dyn Provider>)),
            Vec::new(),
        )
        .unwrap(),
    );
    ws.add_connection(NewConnection {
        name: "shop".into(),
        input: "mssql://reader@replay.invalid:1433/culprit_demo".into(),
        auth: Some(AuthKind::None),
        ..Default::default()
    })
    .expect("save connection");
    let scan = || async { ws.scan("shop", &SnapshotOptions::default()).await.expect("scan") };

    // Without an earlier reading or native reset time, the first scan reports a total as information.
    let first = scan().await;
    assert!(first.snapshot.escalations_before.is_none());
    assert_eq!(escalation(&first).severity, Severity::Low);
    assert_eq!(escalation(&first).actionability, culprit_core::Actionability::Information);
    assert!(escalation(&first).title.contains("2 times since its indexes were loaded"), "{}", escalation(&first).title);

    // A second scan 5ms later does not cover the 7d window, so zero delta over 5ms does not silence the finding.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let second = scan().await;
    assert_eq!(second.snapshot.escalations_before.as_ref().map(|b| &b.taken_at), Some(&first.snapshot.taken_at));
    assert_eq!(escalation(&second).severity, Severity::Low);
    assert_eq!(escalation(&second).actionability, culprit_core::Actionability::Information);
    assert!(!escalation(&second).title.contains(", no observed increase since "), "{}", escalation(&second).title);

    // One more since the earlier scan.
    count.store(3, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let third = scan().await;
    assert_eq!(escalation(&third).severity, Severity::High);
    assert!(escalation(&third).title.contains("escalated to a table lock once since "), "{}", escalation(&third).title);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_scheduled_digest_keeps_raw_escalations_without_reconstructing_native_counter_proof_from_totals() {
    let home = temp_home("digest");
    std::fs::create_dir_all(&home).unwrap();
    let ws = Workspace::open_in(&home);
    let write = |name: &str, json: Vec<u8>| {
        let path = home.join(name);
        std::fs::write(&path, json).unwrap();
        path
    };
    let report = |name: &str, taken_at: &str, escalations: u32, started_at: &str| {
        let mut s = demo(escalations);
        s.taken_at = taken_at.into();
        s.server.started_at = Some(started_at.into());
        write(name, serde_json::to_vec(&s).unwrap())
    };
    let started = "2026-10-11T12:13:03Z";
    let finding = |d: &Digest| d.findings.iter().find(|f| f.id == ID).cloned().expect("raw escalation context");
    let first = ws.digest(None, Some(&report("a.json", "2026-10-12T06:00:00Z", 2, started)), None, 10, 5).unwrap();
    assert_eq!(first.server_started_at.as_deref(), Some(started));
    let first_path = write("first.json", serde_json::to_vec(&first).unwrap());
    for (name, at, count, probe_started) in [
        ("unchanged", "2026-10-12T12:00:00Z", 2, started),
        ("increased", "2026-10-12T18:00:00Z", 5, started),
        ("probe-restarted", "2026-10-13T06:00:00Z", 1, "2026-10-13T01:00:00Z"),
    ] {
        let current = ws.digest(None, Some(&report(name, at, count, probe_started)), Some(&first_path), 10, 5).unwrap();
        let context = finding(&current);
        assert_eq!(context.count, Some(f64::from(count)), "{name}");
        assert_eq!(context.severity, Severity::Low, "{name}");
        assert_eq!(context.actionability, culprit_core::Actionability::Information, "{name}");
        assert!(!context.title.contains("no observed increase"), "{name}");
        assert!(!context.title.contains("since Oct 12"), "{name}");
        assert!(current.since.as_ref().unwrap().again.iter().all(|row| row.id != ID), "{name}");
        assert!(current.top.iter().all(|row| row.id != ID), "{name}");
    }
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn selecting_any_timespan_window_after_a_recent_scan_keeps_escalations_in_that_window() {
    let home = temp_home("all-windows");
    let reports_dir = home.join("reports");
    std::fs::create_dir_all(&reports_dir).unwrap();
    let ws = Workspace::open_in(&home);
    let started = "2026-10-19T12:41:00Z";

    // Case 1: The user opened the app at 06:05 UTC (which ran a 7d scan) and then selected any timespan window
    // (1h, 6h, 24h, 7d, 30d) at 06:06 UTC. Every window, whole days or not, must be respected generically. The
    // 1-minute baseline from 06:05 must not silence the 6 escalations into History for any window.
    let mut scan_0605 = demo(6);
    scan_0605.id = "20261023T060500.000Z".into();
    scan_0605.taken_at = "2026-10-23T06:05:00Z".into();
    scan_0605.server.started_at = Some(started.into());
    scan_0605.options = Some(SnapshotOptions::default().with_window_minutes(7 * 24 * 60));
    let r_0605 = culprit_checks::analyze(scan_0605);
    std::fs::write(reports_dir.join("20261023T060500.000Z.json"), serde_json::to_vec(&r_0605).unwrap()).unwrap();

    for window_minutes in [60_u32, 360, 1440, 7 * 1440, 30 * 1440] {
        let mut selected = demo(6);
        selected.id = format!("20261023T060600.{window_minutes:05}Z");
        selected.taken_at = "2026-10-23T06:06:00Z".into();
        selected.server.started_at = Some(started.into());
        selected.options = Some(SnapshotOptions::default().with_window_minutes(window_minutes));
        let path = reports_dir.join(format!("selected-{window_minutes}.json"));
        std::fs::write(&path, serde_json::to_vec(&selected).unwrap()).unwrap();
        let loaded = ws.analyze_file(&path).unwrap();
        let f = escalation(&loaded);
        assert_eq!(f.severity, Severity::Low, "window {window_minutes}m");
        assert_eq!(f.actionability, culprit_core::Actionability::Information, "window {window_minutes}m");
        assert!(!f.title.contains(", no observed increase since "), "window {window_minutes}m: {}", f.title);
        let _ = std::fs::remove_file(&path);
    }

    // Case 2: The user also scanned 10 hours ago (2026-10-22T20:00:00Z) when the count was already 6 (escalations
    // happened 11 hours ago). Now at 06:06 UTC:
    // - For 1h and 6h, the 10h-old scan proves 0 escalations happened in the last 1h or 6h -> History.
    // - For 24h, 7d and 30d, the 10h-old scan does not cover the window -> Information, not a proven empty window.
    let mut scan_10h_ago = demo(6);
    scan_10h_ago.id = "20261022T200000.000Z".into();
    scan_10h_ago.taken_at = "2026-10-22T20:00:00Z".into();
    scan_10h_ago.server.started_at = Some(started.into());
    scan_10h_ago.options = Some(SnapshotOptions::default().with_window_minutes(7 * 24 * 60));
    let r_10h_ago = culprit_checks::analyze(scan_10h_ago);
    std::fs::write(reports_dir.join("20261022T200000.000Z.json"), serde_json::to_vec(&r_10h_ago).unwrap()).unwrap();

    for window_minutes in [60_u32, 360, 1440, 7 * 1440, 30 * 1440] {
        let mut selected = demo(6);
        selected.id = format!("20261023T060600.{window_minutes:05}Z");
        selected.taken_at = "2026-10-23T06:06:00Z".into();
        selected.server.started_at = Some(started.into());
        selected.options = Some(SnapshotOptions::default().with_window_minutes(window_minutes));
        let path = reports_dir.join(format!("selected-10h-{window_minutes}.json"));
        std::fs::write(&path, serde_json::to_vec(&selected).unwrap()).unwrap();
        let loaded = ws.analyze_file(&path).unwrap();
        let f = escalation(&loaded);
        let expected = if window_minutes <= 360 {
            culprit_core::Actionability::History
        } else {
            culprit_core::Actionability::Information
        };
        assert_eq!(f.actionability, expected, "window {window_minutes}m: {}", f.title);
        let _ = std::fs::remove_file(&path);
    }

    // Case 3: An earlier scan from 12 hours ago (2026-10-22T18:00:00Z) before the escalations happened (count = 0)
    // is also present. Now 24h, 7d and 30d walk past both the 06:05 scan and the 20:00 scan to report 6 new
    // escalations since Oct 22, 18:00 UTC.
    let mut scan_12h_ago = demo(0);
    scan_12h_ago.id = "20261022T180000.000Z".into();
    scan_12h_ago.taken_at = "2026-10-22T18:00:00Z".into();
    scan_12h_ago.server.started_at = Some(started.into());
    scan_12h_ago.options = Some(SnapshotOptions::default().with_window_minutes(7 * 24 * 60));
    let r_12h_ago = culprit_checks::analyze(scan_12h_ago);
    std::fs::write(reports_dir.join("20261022T180000.000Z.json"), serde_json::to_vec(&r_12h_ago).unwrap()).unwrap();

    for window_minutes in [1440_u32, 7 * 1440, 30 * 1440] {
        let mut selected = demo(6);
        selected.id = format!("20261023T060600.{window_minutes:05}Z");
        selected.taken_at = "2026-10-23T06:06:00Z".into();
        selected.server.started_at = Some(started.into());
        selected.options = Some(SnapshotOptions::default().with_window_minutes(window_minutes));
        let path = reports_dir.join(format!("selected-12h-{window_minutes}.json"));
        std::fs::write(&path, serde_json::to_vec(&selected).unwrap()).unwrap();
        let loaded = ws.analyze_file(&path).unwrap();
        let f = escalation(&loaded);
        assert_eq!(f.severity, Severity::High, "window {window_minutes}m");
        assert_eq!(f.actionability, culprit_core::Actionability::NeedsAttention, "window {window_minutes}m");
        assert!(f.title.contains("6 times since Oct 22, 18:00 UTC"), "window {window_minutes}m: {}", f.title);
        let _ = std::fs::remove_file(&path);
    }

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn daily_ci_digest_compares_workload_windows_without_inventing_escalation_counter_continuity() {
    let home = temp_home("daily-ci");
    std::fs::create_dir_all(&home).unwrap();
    let ws = Workspace::open_in(&home);
    let started = "2026-10-19T12:41:00Z";
    let write = |name: &str, json: Vec<u8>| {
        let path = home.join(name);
        std::fs::write(&path, json).unwrap();
        path
    };

    // Day 1: 7d requested, Query Store has 24h of history so far (partial growing coverage).
    let mut day1_snap = demo(2);
    day1_snap.taken_at = "2026-10-22T06:00:00Z".into();
    day1_snap.server.started_at = Some(started.into());
    day1_snap.options = Some(SnapshotOptions::default().with_window_minutes(7 * 24 * 60));
    day1_snap.workload.as_mut().unwrap().window_hours = 24.0;
    day1_snap.workload.as_mut().unwrap().window_start = Some("2026-10-21T06:00:00Z".into());
    day1_snap.workload.as_mut().unwrap().window_end = Some("2026-10-22T06:00:00Z".into());
    day1_snap.bind_identity(day1_snap.database_identity());
    let day1_snap_path = write("day1-snap.json", serde_json::to_vec(&day1_snap).unwrap());
    let day1_digest = ws.digest(None, Some(&day1_snap_path), None, 10, 5).unwrap();
    let day1_digest_path = write("day1-digest.json", serde_json::to_vec(&day1_digest).unwrap());

    // Day 2: 7d requested, Query Store now has 48h of history ("the last 2 days"), and 4 more escalations occurred.
    let mut day2_snap = demo(6);
    day2_snap.taken_at = "2026-10-23T06:01:00Z".into();
    day2_snap.server.started_at = Some(started.into());
    day2_snap.options = Some(SnapshotOptions::default().with_window_minutes(7 * 24 * 60));
    day2_snap.workload.as_mut().unwrap().window_hours = 48.0;
    day2_snap.workload.as_mut().unwrap().window_start = Some("2026-10-21T06:01:00Z".into());
    day2_snap.workload.as_mut().unwrap().window_end = Some("2026-10-23T06:01:00Z".into());
    day2_snap.bind_identity(day2_snap.database_identity());
    let day2_snap_path = write("day2-snap.json", serde_json::to_vec(&day2_snap).unwrap());
    let day2_digest = ws.digest(None, Some(&day2_snap_path), Some(&day1_digest_path), 10, 5).unwrap();
    assert!(day2_digest.since.is_some(), "growing Query Store window (24h -> 48h) must allow digest comparison");
    assert_raw_escalation_context(&day2_digest, 6);

    // Cron jitter does not prevent workload comparison, and does not establish missing index-counter authority.
    let mut day1_24h = day1_snap.clone();
    day1_24h.options = Some(SnapshotOptions::default().with_window_minutes(24 * 60));
    day1_24h.bind_identity(day1_24h.database_identity());
    let day1_24h_path = write("day1-24h.json", serde_json::to_vec(&day1_24h).unwrap());
    let day1_24h_digest = ws.digest(None, Some(&day1_24h_path), None, 10, 5).unwrap();
    let day1_24h_digest_path = write("day1-24h-digest.json", serde_json::to_vec(&day1_24h_digest).unwrap());

    let mut day2_24h = day2_snap.clone();
    day2_24h.taken_at = "2026-10-23T06:05:00Z".into();
    day2_24h.options = Some(SnapshotOptions::default().with_window_minutes(24 * 60));
    day2_24h.workload.as_mut().unwrap().window_hours = 24.0;
    day2_24h.workload.as_mut().unwrap().window_start = Some("2026-10-22T06:05:00Z".into());
    day2_24h.workload.as_mut().unwrap().window_end = Some("2026-10-23T06:05:00Z".into());
    day2_24h.bind_identity(day2_24h.database_identity());
    let day2_24h_path = write("day2-24h.json", serde_json::to_vec(&day2_24h).unwrap());
    let day2_24h_digest = ws.digest(None, Some(&day2_24h_path), Some(&day1_24h_digest_path), 10, 5).unwrap();
    assert!(day2_24h_digest.since.is_some());
    assert_raw_escalation_context(&day2_24h_digest, 6);

    let _ = std::fs::remove_dir_all(home);
}

/// A controlled two-day capture with a saved host identity: the digest keeps only the host's hash.
fn server_scan(taken_at: &str, escalations: u32) -> Snapshot {
    let end = chrono::DateTime::parse_from_rfc3339(taken_at).expect("a time");
    let start = end - chrono::Duration::hours(48);
    let mut s = demo(escalations);
    s.taken_at = taken_at.into();
    s.server.started_at = Some("2026-10-19T12:41:00Z".into());
    s.options = Some(SnapshotOptions::default().with_window_minutes(2 * 24 * 60));
    let workload = s.workload.as_mut().expect("a workload");
    workload.window_hours = 48.0;
    workload.window_start = Some(start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    workload.window_end = Some(taken_at.into());
    s.bind_identity(DatabaseIdentity {
        engine: Engine::Mssql,
        database: s.database_identity().database,
        location: DatabaseLocation::Server { host: "sql.example.invalid".into(), port: 1433, instance: None },
        incarnation: None,
    });
    s
}

#[test]
fn a_redacted_digest_identity_never_supplies_the_missing_per_index_counter_proof() {
    let home = temp_home("real-server");
    std::fs::create_dir_all(&home).unwrap();
    let ws = Workspace::open_in(&home);
    let write = |name: &str, json: Vec<u8>| {
        let path = home.join(name);
        std::fs::write(&path, json).unwrap();
        path
    };
    let scan = |name: &str, taken_at: &str, escalations: u32| {
        write(name, serde_json::to_vec(&server_scan(taken_at, escalations)).unwrap())
    };
    let finding = |d: &Digest| d.findings.iter().find(|f| f.id == ID).cloned().expect("the escalation in the digest");

    let first = ws.digest(None, Some(&scan("a.json", "2026-10-21T06:00:00Z", 2)), None, 10, 5).unwrap();
    let kept = first.identity.as_ref().expect("the digest's identity");
    assert!(matches!(&kept.location, DatabaseLocation::Server { host, .. } if host.starts_with("redacted-")));
    let first_path = write("first.json", serde_json::to_vec(&first).unwrap());

    // Equal raw totals do not establish that this interval was quiet.
    let same = ws.digest(None, Some(&scan("b.json", "2026-10-22T06:00:00Z", 2)), Some(&first_path), 10, 5).unwrap();
    assert_raw_escalation_context(&same, 2);
    assert!(!finding(&same).title.contains("no observed increase"));
    let same_path = write("same.json", serde_json::to_vec(&same).unwrap());

    // A larger total also cannot establish how many events belong to this interval or member.
    let grew = ws.digest(None, Some(&scan("c.json", "2026-10-23T06:00:00Z", 5)), Some(&same_path), 10, 5).unwrap();
    assert_raw_escalation_context(&grew, 5);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_weekend_digest_does_not_assign_undated_counter_totals_to_the_weekend() {
    let home = temp_home("weekend");
    std::fs::create_dir_all(&home).unwrap();
    let ws = Workspace::open_in(&home);
    let write = |name: &str, json: Vec<u8>| {
        let path = home.join(name);
        std::fs::write(&path, json).unwrap();
        path
    };
    let scan = |name: &str, taken_at: &str, escalations: u32| {
        write(name, serde_json::to_vec(&server_scan(taken_at, escalations)).unwrap())
    };

    let friday = ws.digest(None, Some(&scan("fri.json", "2026-10-23T06:00:00Z", 2)), None, 10, 5).unwrap();
    let friday_path = write("friday.json", serde_json::to_vec(&friday).unwrap());

    // A 72-hour interval changes neither the missing native proof nor the original positive counter point.
    let grew = ws.digest(None, Some(&scan("mon.json", "2026-10-26T06:00:00Z", 5)), Some(&friday_path), 10, 5).unwrap();
    assert_raw_escalation_context(&grew, 5);

    let same = ws.digest(None, Some(&scan("mon2.json", "2026-10-26T06:00:00Z", 2)), Some(&friday_path), 10, 5).unwrap();
    assert_raw_escalation_context(&same, 2);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_digest_since_a_report_of_a_real_server_compares_escalations_with_it() {
    let home = temp_home("real-server-report");
    std::fs::create_dir_all(&home).unwrap();
    let ws = Workspace::open_in(&home);
    let scan = |name: &str, taken_at: &str, escalations: u32| {
        let path = home.join(name);
        std::fs::write(&path, serde_json::to_vec(&server_scan(taken_at, escalations)).unwrap()).unwrap();
        path
    };
    let before = scan("a.json", "2026-10-21T06:00:00Z", 2);
    let now = scan("b.json", "2026-10-22T06:00:00Z", 5);
    let digest = ws.digest(None, Some(&now), Some(&before), 10, 5).unwrap();
    let finding = digest.findings.iter().find(|f| f.id == ID).expect("the escalation in the digest");
    assert_eq!(finding.severity, Severity::High);
    assert!(finding.title.contains("3 times since Oct 21, 06:00 UTC"), "{}", finding.title);
    let _ = std::fs::remove_dir_all(home);
}
