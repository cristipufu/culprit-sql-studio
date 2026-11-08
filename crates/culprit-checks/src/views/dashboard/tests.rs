#[test]
fn trend_spans_add_up_to_the_window() {
    let t = Trend { recent_hours: 10.25 / 4.0, baseline_hours: 10.25 * 3.0 / 4.0, ratio: 2.0, recent_ms: 0.0 };
    assert_eq!(trend_spans(&t), ("the last 3 hours".to_string(), "7 hours".to_string()));
    let day = Trend { recent_hours: 24.0, baseline_hours: 48.0, ratio: 2.0, recent_ms: 0.0 };
    assert_eq!(trend_spans(&day), ("the last day".to_string(), "2 days".to_string()));
}

use super::*;

#[test]
fn deadlock_groups_keep_their_identity_and_order_when_labels_change_language() {
    use culprit_core::{Deadlock, DeadlockPattern, DeadlockReport, i18n};
    let mut report = crate::analyze(crate::waits::snapshot_with(Engine::Mssql));
    report.snapshot.deadlocks = Some(DeadlockReport {
        source: "system_health".into(),
        since_hours: 24.0,
        deadlocks: [
            DeadlockPattern::Cycle,
            DeadlockPattern::Parallelism,
            DeadlockPattern::OppositeOrder,
            DeadlockPattern::LockEscalation,
            DeadlockPattern::KeyLookup,
            DeadlockPattern::LockConversion,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, pattern)| {
            let mut deadlock = Deadlock::new("system_health", Some(report.snapshot.taken_at.clone()), vec![], vec![]);
            deadlock.id = format!("event-{index}");
            deadlock.pattern = pattern;
            deadlock.objects = vec!["dbo.items".into()];
            deadlock
        })
        .collect(),
        ..Default::default()
    });
    let english = i18n::scope("en", || super::deadlocks::deadlocks(&report));
    let ids = |tile: &Tile| tile.items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
    assert_eq!(english.items.len(), 3);
    for language in ["de", "fr", "es"] {
        let translated = i18n::scope(language, || super::deadlocks::deadlocks(&report));
        assert_eq!(ids(&translated), ids(&english), "{language}");
        assert_ne!(translated.items[0].label, english.items[0].label);
    }
}

#[test]
fn capability_actions_are_facts_and_their_sentences_stay_intact() {
    use culprit_core::{Capability, MissingCapability};
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    snapshot.workload = None;
    snapshot.server.missing = vec![MissingCapability::sql(
        Capability::QueryStore,
        "La collecte n’est pas disponible.",
        "ALTER DATABASE CURRENT SET QUERY_STORE = ON;",
    )];
    snapshot.server.missing[0].fix = Some("Instructions mention CREATE TABLE example; without authorizing it.".into());
    let report = crate::analyze(snapshot.clone());
    let dashboard = super::dashboard(&report);
    let tile = dashboard.tile(TileKey::Statistics).unwrap();
    assert_eq!(tile.fix_sql.as_deref(), Some("ALTER DATABASE CURRENT SET QUERY_STORE = ON;"));
    assert!(tile.takeaway.ends_with("La collecte n’est pas disponible."));
    assert_eq!(report.summary.no_workload_capability, Some(Capability::QueryStore));
    snapshot.server.missing[0].action = None;
    assert!(super::dashboard(&crate::analyze(snapshot)).tile(TileKey::Statistics).unwrap().fix_sql.is_none());
}

#[test]
fn a_bucket_holding_a_lower_bound_is_left_out_of_the_comparison() {
    // Eight hours at a steady second an hour, except a bucket that holds only a lower bound (a running total fell
    // without a reset). Counted as 50 ms it would make the earlier hours look quieter and today look busier.
    let mut w = crate::waits::snapshot_with(culprit_core::Engine::Postgres).workload.expect("workload");
    let at = |h: usize| format!("2026-10-12T{h:02}:00:00Z");
    w.window_start = Some(at(0));
    let mut time_ms = vec![1_000.0; 8];
    time_ms[2] = 50.0;
    w.timeline = Some(culprit_core::Timeline {
        source: Default::default(),
        starts: (0..8).map(at).collect(),
        end: at(8),
        time_ms,
        executions: vec![0.0; 8],
        unknown: Some((0..8).map(|i| i == 2).collect()),
    });
    let t = trend(&w).expect("a trend");
    assert!((t.ratio - 1.0).abs() < 1e-9, "{}", t.ratio);
}

#[test]
fn the_first_of_equally_severe_findings_leads() {
    use culprit_core::{Category, SubjectKind};
    let mut report = crate::analyze(crate::waits::snapshot_with(Engine::Mssql));
    let make = |key: &str| {
        let mut f = crate::support::finding(
            "failing-query",
            key,
            Category::Queries,
            crate::support::subject(SubjectKind::Query, key),
        );
        f.severity = Severity::High;
        f.title = format!("{key} fails");
        f
    };
    report.findings.retain(|f| f.check != "failing-query");
    report.findings.extend([make("first"), make("second")]);
    assert_eq!(worst_finding(&report, "failing-query").map(|f| f.title.as_str()), Some("first fails"));
}

// Each kind of waiting as the waits tile names it mid-sentence ("Most time goes to disk reads (40%)"), word for word.
#[test]
fn wait_categories_read_mid_sentence() {
    let keys = [
        "rpc",
        "storage",
        "cpu",
        "io",
        "rows",
        "lock",
        "latch",
        "log",
        "network",
        "parallelism",
        "memory",
        "compilation",
        "other",
    ];
    let said: Vec<String> = keys.iter().map(|k| in_sentence(k)).collect();
    assert_eq!(
        said,
        [
            "distributed RPCs",
            "storage I/O",
            "CPU",
            "disk reads",
            "row access",
            "locks",
            "in-memory contention",
            "log writes",
            "sending results",
            "parallel queries",
            "memory",
            "compiling plans",
            "everything else",
        ]
    );
}
