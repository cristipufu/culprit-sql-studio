//! The query checks say only what the statistics show: a plan change needs the times to back it, a spread one outlier
//! can't explain, native temporary work whose cost remains unknown, a wait that reads can't explain, and fixes the server can run. Each test
//! plants one condition on a demo snapshot, with the case that must be flagged next to the ones that must not.

use culprit_checks::analyze;
use culprit_core::{
    Finding, ForcedPlan, NodeKind, Plan, PlanVariant, PlanWarning, QueryFamily, QueryStat, Setting, Severity, Snapshot,
    Spread, TuningAdvice, TuningRecommendation, Unit, WaitStat,
};

fn snapshot(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

fn query<'a>(s: &'a mut Snapshot, id: &str) -> &'a mut QueryStat {
    s.workload.as_mut().expect("workload").queries.iter_mut().find(|q| q.id == id).expect("query")
}

fn find<'a>(findings: &'a [Finding], id: &str) -> Option<&'a Finding> {
    findings.iter().find(|f| f.id == id)
}

fn set(s: &mut Snapshot, name: &str, value: &str) {
    s.settings.retain(|x| !x.name.eq_ignore_ascii_case(name));
    s.settings.push(Setting { name: name.into(), value: value.into(), default_value: None, scope: "server".into() });
}

fn plan(id: &str, runs: f64, mean: f64, first: &str, last: &str) -> PlanVariant {
    serde_json::from_value(serde_json::json!({
        "id": id, "executions": runs, "total_time_ms": runs * mean, "mean_time_ms": mean,
        "first_seen": first, "last_seen": last, "is_forced": false
    }))
    .expect("plan variant")
}

/// The SQL Server demo's query 8 (orders by shipped_at) with these plans, its totals to match, and no timeline (so
/// `regression` stays out of it and `plan-regression` speaks for itself).
fn with_plans(plans: Vec<PlanVariant>) -> Snapshot {
    let mut s = snapshot("mssql-demo.json");
    let q = query(&mut s, "8");
    q.executions = plans.iter().map(|p| p.executions).sum();
    q.total_time_ms = plans.iter().map(|p| p.total_time_ms).sum();
    q.mean_time_ms = q.total_time_ms / q.executions;
    q.trend = None;
    q.plans = plans;
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    s
}

// ── plan-regression ──────────────────────────────────────────────────────────────────────────────

#[test]
fn a_plan_that_replaced_a_faster_one_is_a_plan_regression() {
    let s = with_plans(vec![
        plan("8", 200.0, 9.0, "2026-10-11T07:44:00Z", "2026-10-11T10:44:00Z"),
        plan("47", 100.0, 60.0, "2026-10-11T10:46:00Z", "2026-10-11T12:13:00Z"),
    ]);
    let report = analyze(s);
    let f = find(&report.findings, "plan-regression:8").expect("plan 47 replaced plan 8");
    assert!(f.title.contains("slower after its plan changed"), "{}", f.title);
    assert!(
        f.summary.contains("until Oct 11, 10:44 UTC") && f.summary.contains("since Oct 11, 10:46 UTC"),
        "{}",
        f.summary
    );
    assert!(f.fix.as_ref().unwrap().sql.as_deref().unwrap().contains("@plan_id = 8;"));
}

// The false flags: five quick runs in nine minutes as the "fast plan", and a slower plan that does the same work per
// run (its runs waited on blocking or load while it ran). Neither is a plan that made the query slower.
#[test]
fn a_regression_needs_a_real_baseline_and_more_work_per_run() {
    let s = with_plans(vec![
        plan("8", 5.0, 3.0, "2026-10-11T10:35:00Z", "2026-10-11T10:44:00Z"),
        plan("47", 100.0, 60.0, "2026-10-11T10:46:00Z", "2026-10-11T12:13:00Z"),
    ]);
    assert!(find(&analyze(s).findings, "plan-regression:8").is_none());
    let with_work = |cpu: f64, reads: f64| {
        let mut fast = plan("8", 200.0, 9.0, "2026-10-11T07:44:00Z", "2026-10-11T10:44:00Z");
        (fast.cpu_time_ms, fast.logical_reads) = (Some(200.0 * 4.0), Some(200.0 * 1_000.0));
        let mut slow = plan("47", 100.0, 60.0, "2026-10-11T10:46:00Z", "2026-10-11T12:13:00Z");
        (slow.cpu_time_ms, slow.logical_reads) = (Some(100.0 * cpu), Some(100.0 * reads));
        with_plans(vec![fast, slow])
    };
    assert!(find(&analyze(with_work(4.0, 1_000.0)).findings, "plan-regression:8").is_none());
    // Forty times the reads a run is a regression: the scan of orders the new plan reads for want of an index on
    // shipped_at came with it, so the scan's finding carries it (`scan_carries_its_regression`).
    let report = analyze(with_work(4.0, 40_000.0));
    let carried = report.findings.iter().any(|f| {
        f.id == "plan-regression:8"
            || (f.id.ends_with("shop.orders(shipped_at)") && f.metrics.iter().any(|m| m.label == "Fast plan"))
    });
    assert!(carried, "{:?}", report.findings.iter().map(|f| &f.id).collect::<Vec<_>>());
}

#[test]
fn a_fast_plan_that_came_later_is_not_a_regression() {
    // The slow plan ran from the start; a fast one appeared later and the slow one kept running after it.
    let s = with_plans(vec![
        plan("47", 100.0, 60.0, "2026-10-11T07:44:00Z", "2026-10-11T12:13:00Z"),
        plan("8", 200.0, 9.0, "2026-10-11T10:00:00Z", "2026-10-11T12:00:00Z"),
    ]);
    assert!(find(&analyze(s).findings, "plan-regression:8").is_none());
    // Latest to run is the slow one, but the fast one started after it.
    let s = with_plans(vec![
        plan("47", 100.0, 60.0, "2026-10-11T07:44:00Z", "2026-10-11T12:13:00Z"),
        plan("8", 200.0, 9.0, "2026-10-11T09:00:00Z", "2026-10-11T10:00:00Z"),
    ]);
    assert!(find(&analyze(s).findings, "plan-regression:8").is_none());
}

#[test]
fn overlapping_plan_populations_are_observed_variation_without_a_parameter_cause_or_mutating_fix() {
    let s = with_plans(vec![
        plan("8", 900.0, 9.0, "2026-10-11T07:44:00Z", "2026-10-11T12:12:00Z"),
        plan("47", 100.0, 300.0, "2026-10-11T07:50:00Z", "2026-10-11T12:13:00Z"),
    ]);
    let report = analyze(s);
    assert!(find(&report.findings, "plan-regression:8").is_none(), "plans that overlap aren't a plan change");
    let f = find(&report.findings, "parameter-sensitive:8").expect("recorded plan averages differ");
    assert_eq!(f.actionability, culprit_core::Actionability::Information);
    assert_eq!(f.confidence, culprit_core::Confidence::Medium);
    assert!(matches!(
        &f.facts.runtime_variation,
        Some(culprit_core::FindingRuntimeVariation::PlanAverages { faster_plan, slower_plan })
            if faster_plan == "8" && slower_plan == "47"
    ));
    assert_eq!(f.queries[0].id, "8");
    assert_eq!(
        f.metrics.iter().find(|m| m.is("check.parameter_sensitive.metric.slower_plan_time")).unwrap().value,
        30_000.0
    );
    assert!(f.caveats.iter().any(|c| c.contains("does not establish harmful parameter-sensitive plan reuse")));
    let fix = f.fix.as_ref().unwrap();
    assert!(fix.sql.is_none());
    assert!(fix.cost.is_none());
}

#[test]
fn a_plan_change_on_a_sliver_of_the_workload_is_no_finding() {
    // 6 + 6 runs, a fraction of a percent of database time: 40× slower, and still nothing to act on.
    let s = with_plans(vec![
        plan("8", 6.0, 1.0, "2026-10-11T07:44:00Z", "2026-10-11T08:00:00Z"),
        plan("47", 6.0, 40.0, "2026-10-11T09:00:00Z", "2026-10-11T12:13:00Z"),
    ]);
    assert!(find(&analyze(s).findings, "plan-regression:8").is_none());
}

/// [`with_plans`] over six days of Query Store rather than the demo's four and a half hours.
fn six_days_with_plans(plans: Vec<PlanVariant>) -> Snapshot {
    let mut s = with_plans(plans);
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-10-05T12:13:47Z".into()), 144.0);
    s
}

// The fresh SQL Server demo: a statement that ran its slow plan for three minutes and stopped got "Extra time per day:
// 665 ms", a rate it never ran at, and "Force the fast plan" for a plan that read a table no longer in the catalog.
#[test]
fn a_plan_regression_says_a_day_only_of_a_day_of_slow_runs_and_forces_nothing_onto_a_table_that_is_gone() {
    let burst = || {
        six_days_with_plans(vec![
            plan("8", 450.0, 0.5, "2026-10-11T07:44:00Z", "2026-10-11T07:47:00Z"),
            plan("47", 300.0, 60.0, "2026-10-11T07:48:00Z", "2026-10-11T07:51:00Z"),
        ])
    };
    let report = analyze(burst());
    let f = find(&report.findings, "plan-regression:8").expect("plan 47 replaced plan 8");
    assert!(!f.metrics.iter().any(|m| m.label == "Extra time per day"), "{:?}", f.metrics);
    assert!(
        f.metrics.iter().any(|m| m.label == "Extra time" && (m.value - 300.0 * 59.5).abs() < 1.0),
        "{:?}",
        f.metrics
    );
    assert_eq!(f.impact.time_ms_per_day, None);
    // Two days of the slow plan, still running: its extra time a day over them.
    let s = six_days_with_plans(vec![
        plan("8", 450.0, 0.5, "2026-10-08T07:44:00Z", "2026-10-09T07:47:00Z"),
        plan("47", 300.0, 60.0, "2026-10-09T12:13:47Z", "2026-10-11T12:13:00Z"),
    ]);
    let f = find(&analyze(s).findings, "plan-regression:8").cloned().expect("plan 47 replaced plan 8");
    let a_day = f.metrics.iter().find(|m| m.label == "Extra time per day").expect("a day of slow runs").value;
    assert!((a_day - 300.0 * 59.5 / 2.0).abs() < 1.0, "{a_day}");

    // The table the statement read is gone: nothing to force.
    let mut s = burst();
    s.tables.retain(|t| t.name != "orders");
    let f = find(&analyze(s).findings, "plan-regression:8").cloned().expect("plan 47 replaced plan 8");
    let fix = f.fix.unwrap();
    assert!(fix.sql.is_none(), "{:?}", fix.sql);
    assert!(
        fix.summary.starts_with("shop.orders is no longer in the catalog") && !fix.summary.contains("Force"),
        "{}",
        fix.summary
    );
}

// What plan-regression says, word for word: a slow plan still running over two days, one that ran for three minutes,
// one that stopped days ago, one whose table is gone, and one where SQL Server corrects regressions itself. Its
// messages come from locales/en.json.
#[test]
fn a_plan_regression_says_what_changed_in_english() {
    assert_eq!(
        crate::info("plan-regression"),
        "Plan regressions\n\
        Queries whose current plan is much slower than a plan they used before."
    );
    let running = || {
        six_days_with_plans(vec![
            plan("8", 450.0, 0.5, "2026-10-08T07:44:00Z", "2026-10-09T07:47:00Z"),
            plan("47", 300.0, 60.0, "2026-10-09T12:13:47Z", "2026-10-11T12:13:00Z"),
        ])
    };
    let burst = || {
        six_days_with_plans(vec![
            plan("8", 450.0, 0.5, "2026-10-11T07:44:00Z", "2026-10-11T07:47:00Z"),
            plan("47", 300.0, 60.0, "2026-10-11T07:48:00Z", "2026-10-11T07:51:00Z"),
        ])
    };
    let mut stopped = six_days_with_plans(vec![
        plan("8", 450.0, 0.5, "2026-10-06T07:44:00Z", "2026-10-07T07:47:00Z"),
        plan("47", 300.0, 60.0, "2026-10-07T12:13:47Z", "2026-10-08T12:13:00Z"),
    ]);
    let q = query(&mut stopped, "8");
    (q.first_seen, q.last_seen) = (Some("2026-10-06T07:44:00Z".into()), Some("2026-10-08T12:13:00Z".into()));
    let mut gone = burst();
    gone.tables.retain(|t| t.name != "orders");
    let mut automatic = running();
    automatic.tuning = Some(TuningAdvice {
        automatic_tuning: vec![culprit_core::AutomaticTuningOption {
            name: "FORCE_LAST_GOOD_PLAN".into(),
            desired: "ON".into(),
            actual: "ON".into(),
            reason: None,
        }],
        ..Default::default()
    });
    let said = |s: Snapshot| crate::words(find(&analyze(s).findings, "plan-regression:8").expect("47 replaced 8"));
    assert_eq!(
        [said(running()), said(burst()), said(stopped), said(gone), said(automatic)],
        [
            "A read from orders got 120× slower after its plan changed · Low · High\n\
                The associated statements used 18.2 s of database time over the last 6 days (34%). Query Store saw this query run with plan 8 at 0.50 ms on average until Oct 9, 07:47 UTC, and with plan 47 at 60 ms since Oct 9, 12:13 UTC (300 runs). The plan choice changed, typically after a statistics update, a schema change or a recompile with unusual parameter values.\n\
                impact: 120× slower\n\
                metric: Fast plan\n\
                metric: Current plan\n\
                metric: Extra time per day\n\
                fix: Force the fast plan now to stop the bleeding, then find out why the optimizer changed its mind (statistics, parameter values, schema). Unforce once the cause is fixed.\n\
                sql: EXEC sp_query_store_force_plan @query_id = 8, @plan_id = 8;\n\
                verify: The query's average duration in Query Store should return to the fast plan's level within an interval or two.\n\
                caveat: A forced plan can fail to apply after schema changes; Query Store reports that in sys.query_store_plan.last_force_failure_reason_desc.",
            "A read from orders got 120× slower after its plan changed · Low · High\n\
                The associated statements used 18.2 s of database time over the last 6 days (34%). Query Store saw this query run with plan 8 at 0.50 ms on average until Oct 11, 07:47 UTC, and with plan 47 at 60 ms since Oct 11, 07:48 UTC (300 runs). The plan choice changed, typically after a statistics update, a schema change or a recompile with unusual parameter values.\n\
                impact: 120× slower\n\
                metric: Fast plan\n\
                metric: Current plan\n\
                metric: Extra time\n\
                fix: Force the fast plan now to stop the bleeding, then find out why the optimizer changed its mind (statistics, parameter values, schema). Unforce once the cause is fixed.\n\
                sql: EXEC sp_query_store_force_plan @query_id = 8, @plan_id = 8;\n\
                verify: The query's average duration in Query Store should return to the fast plan's level within an interval or two.\n\
                caveat: A forced plan can fail to apply after schema changes; Query Store reports that in sys.query_store_plan.last_force_failure_reason_desc.",
            "A read from orders got 120× slower after its plan changed · Low · High\n\
                Query Store saw this query run with plan 8 at 0.50 ms on average until Oct 7, 07:47 UTC, and with plan 47 at 60 ms between Oct 7, 12:13 and Oct 8, 12:13 UTC (300 runs). The plan choice changed, typically after a statistics update, a schema change or a recompile with unusual parameter values.\n\
                impact: 120× slower\n\
                metric: Fast plan\n\
                metric: Last plan\n\
                metric: Extra time per day\n\
                fix: Force the fast plan now to stop the bleeding, then find out why the optimizer changed its mind (statistics, parameter values, schema). Unforce once the cause is fixed.\n\
                sql: EXEC sp_query_store_force_plan @query_id = 8, @plan_id = 8;\n\
                verify: Nothing to check unless it runs again: a later scan that sees it run lists this finding as current again.\n\
                caveat: This query has not run since 2026-10-08 12:13 (3.0 days before this scan's data ends). A deploy may have changed or fixed it; check that the current version of this code doesn't have the same problem.\n\
                caveat: A forced plan can fail to apply after schema changes; Query Store reports that in sys.query_store_plan.last_force_failure_reason_desc.",
            "A read from orders got 120× slower after its plan changed · Low · High\n\
                The associated statements used 18.2 s of database time over the last 6 days (34%). Query Store saw this query run with plan 8 at 0.50 ms on average until Oct 11, 07:47 UTC, and with plan 47 at 60 ms since Oct 11, 07:48 UTC (300 runs). The plan choice changed, typically after a statistics update, a schema change or a recompile with unusual parameter values.\n\
                impact: 120× slower\n\
                metric: Fast plan\n\
                metric: Last plan\n\
                metric: Extra time\n\
                fix: shop.orders is no longer in the catalog, so the statement can't run as it was, and plan 8 can't be forced onto it: a forced plan that names a table or an index that is gone fails to apply. Nothing to force; if a statement on the table that replaced it is slow, its own findings say so.\n\
                verify: Nothing to check unless the statement runs again.",
            "A read from orders got 120× slower after its plan changed · Low · High\n\
                The associated statements used 18.2 s of database time over the last 6 days (34%). Query Store saw this query run with plan 8 at 0.50 ms on average until Oct 9, 07:47 UTC, and with plan 47 at 60 ms since Oct 9, 12:13 UTC (300 runs). The plan choice changed, typically after a statistics update, a schema change or a recompile with unusual parameter values.\n\
                impact: 120× slower\n\
                metric: Fast plan\n\
                metric: Current plan\n\
                metric: Extra time per day\n\
                fix: Force the fast plan now to stop the bleeding, then find out why the optimizer changed its mind (statistics, parameter values, schema). Unforce once the cause is fixed. Automatic plan correction (FORCE_LAST_GOOD_PLAN) is on for this database: SQL Server forces the last good plan itself once it is sure a regression costs enough (sys.dm_db_tuning_recommendations lists the ones it found), and unforces a plan it forced that turns out slower. A plan forced by hand stays forced until someone unforces it, whatever changes after. It hasn't forced plan 8 for this one so far.\n\
                sql: EXEC sp_query_store_force_plan @query_id = 8, @plan_id = 8;\n\
                verify: The query's average duration in Query Store should return to the fast plan's level within an interval or two.\n\
                caveat: A forced plan can fail to apply after schema changes; Query Store reports that in sys.query_store_plan.last_force_failure_reason_desc.",
        ]
    );
}

// ── regression ───────────────────────────────────────────────────────────────────────────────────

/// The SQL Server demo with its Query Store timeline's 15-minute buckets made `hours` long: query 8 gets slower at
/// the start of bucket 13, and its plan 47 first runs a minute into it, after plan 8.
fn stretched(hours: f64) -> Snapshot {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    let mut s = snapshot("mssql-demo.json");
    let w = s.workload.as_mut().unwrap();
    let t = w.timeline.as_mut().unwrap();
    let base = epoch_seconds(&t.starts[0]).unwrap();
    let at = |i: usize| base + i as f64 * hours * 3600.0;
    for (i, start) in t.starts.iter_mut().enumerate() {
        *start = iso_from_epoch(at(i));
    }
    let n = t.starts.len();
    t.end = iso_from_epoch(at(n));
    (w.window_start, w.window_end, w.window_hours) =
        (Some(iso_from_epoch(at(0))), Some(t.end.clone()), n as f64 * hours);
    let q = query(&mut s, "8");
    for p in &mut q.plans {
        (p.first_seen, p.last_seen) = match p.id.as_str() {
            "47" => (Some(iso_from_epoch(at(13) + 60.0)), Some(iso_from_epoch(at(n) - 60.0))),
            _ => (Some(iso_from_epoch(at(0) + 60.0)), Some(iso_from_epoch(at(13) - 60.0))),
        };
    }
    s
}

// What regression says, word for word, beyond the demo's (a plan change on SQL Server, over 89 minutes): over hours
// and over a day, with SQL Server correcting plans itself, a plan change it can't force, no plan change among several
// plans or with one, a server that had just started, a trend from Culprit's own scans, and a slow query log's time.
// Its messages come from locales/en.json.
#[test]
fn a_regression_says_when_and_how_much_in_english() {
    assert_eq!(
        crate::info("regression"),
        "Queries that got slower\n\
        Queries whose runs became much slower at some point in the window, with when it started and whether a plan change coincides. Needs Query Store history or a few earlier scans."
    );
    let mut automatic = stretched(4.0);
    automatic.tuning = Some(TuningAdvice {
        automatic_tuning: vec![culprit_core::AutomaticTuningOption {
            name: "FORCE_LAST_GOOD_PLAN".into(),
            desired: "ON".into(),
            actual: "ON".into(),
            reason: None,
        }],
        ..Default::default()
    });
    let mut unforceable = stretched(1.0);
    query(&mut unforceable, "8").plans.iter_mut().find(|p| p.id == "8").unwrap().id = "a8".into();
    let mut no_new_plan = snapshot("mssql-demo.json");
    query(&mut no_new_plan, "8").plans.iter_mut().find(|p| p.id == "47").unwrap().first_seen =
        Some("2026-10-11T07:40:00Z".into());
    no_new_plan.server.started_at = Some("2026-10-11T10:00:00Z".into());
    let mut one_plan = snapshot("mssql-demo.json");
    query(&mut one_plan, "8").plans.retain(|p| p.id == "8");
    one_plan.workload.as_mut().unwrap().timeline.as_mut().unwrap().source = culprit_core::TimelineSource::Scans;
    let mut with_logging_floor = snapshot("mssql-demo.json");
    with_logging_floor.workload.as_mut().unwrap().min_logged_ms = Some(100.0);
    assert_eq!(with_logging_floor.workload.as_ref().unwrap().kind, Some(culprit_core::WorkloadKind::QueryStore));
    let said = |s: Snapshot| {
        let found = crate::run("regression", &s);
        crate::words(found.iter().find(|f| f.id == "regression:8").expect("query 8 got slower"))
    };
    assert_eq!(
        [
            said(stretched(1.0)),
            said(automatic),
            said(unforceable),
            said(no_new_plan),
            said(one_plan),
            said(with_logging_floor),
        ],
        [
            "A read from orders got 3.4× slower on Oct 11 at 20:30 UTC, when its plan changed · Low · High\n\
                Until Oct 11, 20:30 UTC it averaged 9.5 ms per run; since then 32 ms, over 93 runs: 2.1 s of extra database time in 6 hours. Plan 47 first ran on Oct 11 at 20:31 UTC and averages 32 ms; plan 8 averaged 9.5 ms.\n\
                impact: 3.4× slower\n\
                metric: Before\n\
                metric: Since then\n\
                metric: Extra time\n\
                metric: Started: Oct 11, 20:30 UTC\n\
                metric: Plan change: 8 → 47\n\
                fix: Force plan 8 to stop the bleeding, then find out why the optimizer switched (statistics, parameter values, schema). Unforce once the cause is fixed.\n\
                sql: EXEC sp_query_store_force_plan @query_id = 8, @plan_id = 8;\n\
                verify: Re-scan after the fix: this query's trend in Queries should drop back to about 9.5 ms per run.",
            "A read from orders got 3.4× slower on Oct 13 at 11:30 UTC, when its plan changed · Low · High\n\
                Until Oct 13, 11:30 UTC it averaged 9.5 ms per run; since then 32 ms, over 93 runs: about 2.1 s of extra database time a day. Plan 47 first ran on Oct 13 at 11:31 UTC and averages 32 ms; plan 8 averaged 9.5 ms.\n\
                impact: 3.4× slower\n\
                metric: Before\n\
                metric: Since then\n\
                metric: Extra time per day\n\
                metric: Started: Oct 13, 11:30 UTC\n\
                metric: Plan change: 8 → 47\n\
                fix: Force plan 8 to stop the bleeding, then find out why the optimizer switched (statistics, parameter values, schema). Unforce once the cause is fixed. Automatic plan correction (FORCE_LAST_GOOD_PLAN) is on for this database: SQL Server forces the last good plan itself once it is sure a regression costs enough (sys.dm_db_tuning_recommendations lists the ones it found), and unforces a plan it forced that turns out slower. A plan forced by hand stays forced until someone unforces it, whatever changes after.\n\
                sql: EXEC sp_query_store_force_plan @query_id = 8, @plan_id = 8;\n\
                verify: Re-scan after the fix: this query's trend in Queries should drop back to about 9.5 ms per run.",
            "A read from orders got 3.4× slower on Oct 11 at 20:30 UTC, when its plan changed · Low · High\n\
                Until Oct 11, 20:30 UTC it averaged 9.5 ms per run; since then 32 ms, over 93 runs: 2.1 s of extra database time in 6 hours. Plan 47 first ran on Oct 11 at 20:31 UTC and averages 32 ms; plan a8 averaged 9.5 ms.\n\
                impact: 3.4× slower\n\
                metric: Before\n\
                metric: Since then\n\
                metric: Extra time\n\
                metric: Started: Oct 11, 20:30 UTC\n\
                metric: Plan change: a8 → 47\n\
                fix: The plan changed when it got slower. Compare the two plans (Queries → Plans), refresh statistics on the tables it reads, and check the parameter values the slow runs use.\n\
                verify: Re-scan after the fix: this query's trend in Queries should drop back to about 9.5 ms per run.",
            "A read from orders got 3.4× slower on Oct 11 at 10:45 UTC · Low · Low\n\
                Until Oct 11, 10:45 UTC it averaged 9.5 ms per run; since then 32 ms, over 93 runs: 2.1 s of extra database time in 89 minutes. No new plan started when it got slower, so look at what else changed then: a deploy that changed this statement or its parameters, a jump in data volume, a statistics update, or new blocking.\n\
                impact: 3.4× slower\n\
                metric: Before\n\
                metric: Since then\n\
                metric: Extra time\n\
                metric: Started: Oct 11, 10:45 UTC\n\
                fix: Find what changed on Oct 11 at 10:45 UTC: a deploy touching this statement, its parameters or its indexes; data growth in the tables it reads; a statistics update; or blocking (Live shows current waits). Open the query to compare its plan with what you expect.\n\
                verify: Re-scan after the fix: this query's trend in Queries should drop back to about 9.5 ms per run.\n\
                caveat: SQL Server started on Oct 11 at 10:00 UTC, just before it got slower: the first hours after a start read from disk what was in memory before. If it stays slow once the cache is warm, it's a real change.",
            "A read from orders got 3.4× slower on Oct 11 at 10:45 UTC · Low · Medium\n\
                Until Oct 11, 10:45 UTC it averaged 9.5 ms per run; since then 32 ms, over 93 runs: 2.1 s of extra database time in 89 minutes. This scan does not establish which plan the earlier and later runs used, so a plan change remains possible. Compare plans and parameter values, and check for changed data volume, statistics, blocking or application code around that time.\n\
                impact: 3.4× slower\n\
                metric: Before\n\
                metric: Since then\n\
                metric: Extra time\n\
                metric: Started: Oct 11, 10:45 UTC\n\
                fix: Find what changed on Oct 11 at 10:45 UTC: a deploy touching this statement, its parameters or its indexes; data growth in the tables it reads; a statistics update; or blocking (Live shows current waits). Open the query to compare its plan with what you expect.\n\
                verify: Re-scan after the fix: this query's trend in Queries should drop back to about 9.5 ms per run.\n\
                caveat: Culprit built this trend from its own scans. Each point covers the time between two scans, so the start time is only as precise as how often you scan.",
            r#"A read from orders got 3.4× slower on Oct 11 at 10:45 UTC, when its plan changed · Low · High
Until Oct 11, 10:45 UTC it averaged 9.5 ms per run; since then 32 ms, over 93 runs: 2.1 s of extra database time in 89 minutes. Plan 47 first ran on Oct 11 at 10:46 UTC and averages 32 ms; plan 8 averaged 9.5 ms.
impact: 3.4× slower
metric: Before
metric: Since then
metric: Extra time
metric: Started: Oct 11, 10:45 UTC
metric: Plan change: 8 → 47
fix: Force plan 8 to stop the bleeding, then find out why the optimizer switched (statistics, parameter values, schema). Unforce once the cause is fixed.
sql: EXEC sp_query_store_force_plan @query_id = 8, @plan_id = 8;
verify: Re-scan after the fix: this query's trend in Queries should drop back to about 9.5 ms per run."#,
        ]
    );
}

// ── one plan story per query ─────────────────────────────────────────────────────────────────────

fn recommend(query: &str, good: &str, bad: &str) -> TuningAdvice {
    TuningAdvice {
        recommendations: vec![TuningRecommendation {
            name: "PR_1".into(),
            kind: "FORCE_LAST_GOOD_PLAN".into(),
            state: Some("Active".into()),
            query_id: Some(query.into()),
            regressed_plan_id: Some(bad.into()),
            recommended_plan_id: Some(good.into()),
            estimated_gain: Some(0.7),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn sql_servers_recommendation_carries_the_regression_of_its_query() {
    // The demo's query 8 got 3.4× slower when plan 47 replaced plan 8 (`regression`); SQL Server recommends plan 8.
    let mut s = snapshot("mssql-demo.json");
    s.tuning = Some(recommend("8", "8", "47"));
    let report = analyze(s);
    let about_8: Vec<&str> = report
        .findings
        .iter()
        .filter(|f| f.queries.iter().any(|q| q.id == "8"))
        .filter(|f| matches!(f.check.as_str(), "regression" | "plan-regression" | "tuning-advice"))
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(about_8, vec!["tuning-advice:force:8"]);
    let f = find(&report.findings, "tuning-advice:force:8").unwrap();
    assert!(f.title.contains("(estimated 70% less CPU per run)"), "{}", f.title);
    assert!(f.summary.contains("Query Store's timeline: until Oct 11, 10:45 UTC it averaged"), "{}", f.summary);
    assert!(f.metrics.iter().any(|m| m.label == "Plan change"), "{:?}", f.metrics);
    assert!(f.fix.as_ref().unwrap().sql.as_deref().unwrap().contains("@plan_id = 8"));
    let alone = analyze(snapshot("mssql-demo.json"));
    let regression = alone
        .findings
        .iter()
        .find(|r| r.queries.iter().any(|q| q.id == "8") && matches!(r.check.as_str(), "regression" | "plan-regression"))
        .expect("the regression on its own");
    assert_eq!(f.severity, regression.severity, "the regression's severity carries over");
}

// The recommendation tells the regression's timeline from its facts, whatever language the report is said in.
#[test]
fn sql_servers_recommendation_tells_the_regression_in_another_language() {
    let mut s = snapshot("mssql-demo.json");
    s.tuning = Some(recommend("8", "8", "47"));
    let report = culprit_core::i18n::scope("fr", || analyze(s));
    let f = find(&report.findings, "tuning-advice:force:8").unwrap();
    assert!(f.summary.contains("Chronologie de Query Store : jusqu’au "), "{}", f.summary);
}

// A heavy statement's title says what it cost in place of its share, whatever language the report is said in.
#[test]
fn a_heavy_statements_title_says_its_cost_in_another_language() {
    let english = analyze(snapshot("postgres-demo.json"));
    let french = culprit_core::i18n::scope("fr", || analyze(snapshot("postgres-demo.json")));
    let titled = |r: &culprit_core::Report, words: &str| {
        r.findings.iter().filter(|f| f.check == "heavy-query" && f.title.contains(words)).count()
    };
    assert!(titled(&english, " of database time over ") > 0);
    assert_eq!(titled(&french, " de temps de base de données sur "), titled(&english, " of database time over "));
}

#[test]
fn native_plan_recommendations_keep_their_authorized_fix_beside_an_unresolved_variation_candidate() {
    let mut s = with_plans(vec![
        plan("8", 900.0, 9.0, "2026-10-11T07:44:00Z", "2026-10-11T12:12:00Z"),
        plan("47", 100.0, 300.0, "2026-10-11T07:50:00Z", "2026-10-11T12:13:00Z"),
    ]);
    s.tuning = Some(recommend("8", "47", "8"));
    let report = analyze(s);
    let recommendation =
        find(&report.findings, "tuning-advice:force:8").expect("native recommendation remains visible");
    assert_eq!(recommendation.queries[0].id, "8");
    assert_eq!(recommendation.facts.recommended_plan.as_ref().unwrap().plan, "47");
    assert!(recommendation.fix.as_ref().unwrap().sql.as_deref().unwrap().contains("@plan_id = 47"));
    assert_eq!(recommendation.actionability, culprit_core::Actionability::NeedsAttention);
    let candidate = find(&report.findings, "parameter-sensitive:8").expect("observed populations remain visible");
    assert_eq!(candidate.actionability, culprit_core::Actionability::Information);
    assert!(candidate.fix.as_ref().unwrap().sql.is_none());
    assert!(
        matches!(&candidate.facts.runtime_variation, Some(culprit_core::FindingRuntimeVariation::PlanAverages { faster_plan, slower_plan }) if faster_plan == "8" && slower_plan == "47")
    );
}

// ── unstable-query ───────────────────────────────────────────────────────────────────────────────

/// Postgres demo's customer lookup (8.2K runs, 0.02 ms) with this spread.
fn unstable(mean: f64, stddev: f64, max: f64, runs: f64) -> Snapshot {
    let mut s = snapshot("postgres-demo.json");
    let q = query(&mut s, "3432126458605143571");
    q.executions = runs;
    q.mean_time_ms = mean;
    q.total_time_ms = mean * runs;
    q.stddev_time_ms = Some(stddev);
    q.max_time_ms = Some(max);
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    s
}

fn spread_of(xs: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    let n: f64 = xs.iter().map(|(_, k)| k).sum();
    let mean = xs.iter().map(|(x, k)| x * k).sum::<f64>() / n;
    let var = xs.iter().map(|(x, k)| (x - mean).powi(2) * k).sum::<f64>() / n;
    let max = xs.iter().map(|(x, _)| *x).fold(0.0, f64::max);
    (mean, var.sqrt(), max, n)
}

#[test]
fn one_slow_run_among_thousands_is_not_an_unstable_query() {
    // 10,000 runs at 2 ms and one at 5 s: a lock wait during a migration, not a query that "sometimes" takes 5 s.
    let (mean, sd, max, n) = spread_of(&[(2.0, 10_000.0), (5000.0, 1.0)]);
    let ids: Vec<String> = analyze(unstable(mean, sd, max, n)).findings.into_iter().map(|f| f.id).collect();
    assert!(!ids.iter().any(|id| id.starts_with("unstable-query:")), "{ids:?}");
}

#[test]
fn many_slow_runs_are_an_unstable_query_rated_by_their_time() {
    // 1% of the runs take 5 s.
    let (mean, sd, max, n) = spread_of(&[(2.0, 9_900.0), (5000.0, 100.0)]);
    let report = analyze(unstable(mean, sd, max, n));
    let f = find(&report.findings, "unstable-query:3432126458605143571").expect("an unstable query");
    assert!(f.title.contains("but some runs take far longer (up to 5.0 s)"), "{}", f.title);
    assert!(
        f.summary.starts_with("Same statement, very different run times"),
        "PostgreSQL keeps no plan identity: {}",
        f.summary
    );
    assert!(f.caveats.iter().any(|c| c.contains("could be from any time")), "{:?}", f.caveats);
    let verify = f.verify.join(" ");
    assert!(
        verify.contains("before the change and again after it") && verify.contains("same logging threshold"),
        "{verify}"
    );
    assert!(verify.contains("Compare their durations and any recorded lock waits"), "{verify}");
    assert!(
        verify.contains("historical maximum and standard deviation do not isolate runs after the change"),
        "{verify}"
    );
    assert!(verify.contains("use the new log samples"), "{verify}");
    let extra = f.metrics.iter().find(|m| m.label.starts_with("Slow runs' extra time")).unwrap().value;
    assert!((extra - 100.0 * (5000.0 - mean)).abs() / extra < 0.1, "{extra}");
}

/// A PostgreSQL plan of an INSERT of the values it is given: a ModifyTable over a Result, no table or index read.
fn insert_values_plan() -> Plan {
    let (plan_facts, node_facts) = (
        serde_json::to_value(culprit_core::PlanFacts::default()).unwrap(),
        serde_json::to_value(culprit_core::PlanNodeFacts::default()).unwrap(),
    );
    serde_json::from_value(serde_json::json!({
        "engine": "postgres", "kind": "generic", "total_cost": 0.01, "warnings": [], "index_hints": [], "props": {},
        "facts": plan_facts,
        "root": {
            "id": 0, "op": "ModifyTable", "kind": "modify", "object": "public.app_salts", "detail": "Insert",
            "est_rows": 0.0, "est_executions": 1.0, "subtree_cost": 0.01, "self_cost": 0.0,
            "warnings": [], "props": {"Conflict Resolution": "NOTHING"}, "facts": node_facts.clone(),
            "children": [{
                "id": 1, "op": "Result", "kind": "compute", "est_rows": 1.0, "est_executions": 1.0,
                "subtree_cost": 0.01, "self_cost": 0.01, "warnings": [], "props": {}, "facts": node_facts,
                "children": []
            }]
        }
    }))
    .expect("plan")
}

#[test]
fn an_insert_of_its_values_whose_runs_swing_waited() {
    // `INSERT … VALUES … ON CONFLICT DO NOTHING` reads no table: its values can't make a run do more, so no parameter
    // sensitivity, generic plan or statistics target explains its slow runs.
    let (mean, sd, max, n) = spread_of(&[(2.0, 9_900.0), (5000.0, 100.0)]);
    let mut s = unstable(mean, sd, max, n);
    let q = query(&mut s, "3432126458605143571");
    q.text = "INSERT INTO app_salts (app_id, date, salt) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING".into();
    q.plan = Some(insert_values_plan());
    let report = analyze(s);
    let f = find(&report.findings, "unstable-query:3432126458605143571").expect("an unstable insert");
    assert!(f.summary.contains("Its plan reads no table"), "{}", f.summary);
    assert!(f.summary.contains("inserting the same key"), "{}", f.summary);
    assert!(!f.summary.contains("parameter"), "{}", f.summary);
    let fix = f.fix.as_ref().unwrap();
    assert!(fix.sql.is_none(), "{:?}", fix.sql);
    assert!(!fix.summary.contains("plan_cache_mode") && fix.summary.contains("wait"), "{}", fix.summary);
}

#[test]
fn an_unstable_querys_share_is_its_slow_runs_time() {
    // The share and the time a day beside the finding (its row's bar, the dashboard, the digest, an agent's
    // db_time_pct) are what its label says: the slow runs', not the statement's 25% that the label calls "<1%".
    let (mean, sd, max, n) = spread_of(&[(2.0, 9_900.0), (5000.0, 100.0)]);
    let mut s = unstable(mean, sd, max, n);
    s.workload.as_mut().unwrap().window_hours = 48.0;
    let report = analyze(s);
    let f = find(&report.findings, "unstable-query:3432126458605143571").expect("an unstable query");
    let share = f.impact.db_time_share.expect("the slow runs' share");
    assert_eq!(f.impact.label, format!("{} of DB time in slow runs", culprit_core::format::percent(share)));
    let extra = f.metrics.iter().find(|m| m.label.starts_with("Slow runs' extra time")).unwrap().value;
    let w = report.snapshot.workload.as_ref().unwrap();
    let q = w.query("3432126458605143571").unwrap();
    assert!((share - w.share_of(extra, q)).abs() < 1e-12, "{share}");
    assert!(share < w.share_of(q.total_time_ms, q), "below the statement's own share");
    let a_day = f.impact.time_ms_per_day.expect("48 hours of data");
    assert!((a_day - extra / 2.0).abs() < 1e-6, "{a_day} vs {extra} over two days");
}

// The aptabase reading: an insert taking a quarter of database time lost its heavy-query finding to the unstable-query
// one, whose slow runs were under 1% of it, so nothing said what the statement costs every day. A finding explains a
// heavy query away only when it accounts for its time.
#[test]
fn slow_runs_that_are_a_part_of_a_heavy_insert_leave_its_heavy_query() {
    let (mean, sd, max, n) = spread_of(&[(20.0, 19_940.0), (5000.0, 20.0)]);
    let mut s = unstable(mean, sd, max, n);
    let q = query(&mut s, "3432126458605143571");
    q.text = "INSERT INTO app_salts (app_id, date, salt) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING".into();
    q.plan = Some(insert_values_plan());
    let report = analyze(s);
    let unstable = find(&report.findings, "unstable-query:3432126458605143571").expect("an unstable insert");
    let heavy = find(&report.findings, "heavy-query:3432126458605143571").expect("its everyday cost, still said");
    assert!(
        unstable.impact.db_time_share.unwrap() < heavy.impact.db_time_share.unwrap() * 0.5,
        "{:?} vs {:?}",
        unstable.impact,
        heavy.impact
    );
    // A finding that accounts for a statement's time still stands for it: the demo's sort that spills.
    let report = analyze(snapshot("postgres-demo.json"));
    assert!(find(&report.findings, "temp-spill:9065391607317686058").is_some());
    assert!(find(&report.findings, "heavy-query:9065391607317686058").is_none());
}

/// [`insert_values_plan`] writing `object`.
fn insert_plan_into(object: &str) -> Plan {
    let mut plan = insert_values_plan();
    plan.root.object = Some(object.into());
    plan
}

// The aptabase reading: "its plan reads no table, so different values don't make a run do more work" for an insert
// whose foreign key checks every new row against apps, a read and a row lock its plan doesn't show; "the log at
// commit" for PostgreSQL, whose statement time ends before the commit; a guess said as fact ("the slow runs were
// waiting, for…"); "Culprit can't tell which values were slow" right after saying values don't matter; and "another
// transaction inserting the same key" whatever the table's unique keys, even one the database generates.
#[test]
fn an_unstable_insert_says_what_its_foreign_keys_lock() {
    let (mean, sd, max, n) = spread_of(&[(2.0, 9_900.0), (5000.0, 100.0)]);
    let mut s = unstable(mean, sd, max, n);
    let q = query(&mut s, "3432126458605143571");
    q.text = "INSERT INTO order_items (order_id, product_id, quantity) VALUES ($1, $2, $3)".into();
    q.plan = Some(insert_plan_into("public.order_items"));
    let report = analyze(s.clone());
    let f = find(&report.findings, "unstable-query:3432126458605143571").expect("an unstable insert");
    assert!(
        f.summary.contains(
            "Its plan reads no table (it writes the values it is given), but its foreign keys on product_id and order_id check each row it inserts against products and orders: the referenced row is looked up and share-locked (FOR KEY SHARE)."
        ),
        "{}",
        f.summary
    );
    // Its only unique key is the id the sequence gives it: no other transaction inserts the same one.
    assert!(
        f.summary.contains("The slow runs may have been waiting for a transaction that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of products and orders, for a lock on the table"),
        "{}",
        f.summary
    );
    for wrong in ["different values don't", "at commit", "were waiting, for", "same key"] {
        assert!(!f.summary.contains(wrong), "{wrong}: {}", f.summary);
    }
    let fix = f.fix.as_ref().unwrap();
    assert!(fix.summary.starts_with("Find what the slow runs wait for while they happen"), "{}", fix.summary);
    assert!(
        fix.summary.contains(
            "A transaction that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of products and orders holds this one until it commits or rolls back"
        ),
        "{}",
        fix.summary
    );
    assert!(!fix.summary.contains("same key"), "{}", fix.summary);
    let caveat = f.caveats.iter().find(|c| c.contains("could be from any time")).unwrap();
    assert!(!caveat.contains("which values") && caveat.ends_with("how many calls were slow."), "{caveat}");

    // A unique key on the columns it sets: another transaction inserting the same order and product holds it.
    let pkey = s.indexes.iter().find(|i| i.name == "order_items_pkey").cloned().expect("demo index");
    s.indexes.push(culprit_core::IndexStat {
        filter_facts: None,
        name: "order_items_order_product_key".into(),
        columns: vec!["order_id".into(), "product_id".into()],
        is_primary_key: false,
        ..pkey
    });
    let report = analyze(s);
    let f = find(&report.findings, "unstable-query:3432126458605143571").expect("an unstable insert");
    assert!(
        f.summary.contains(
            "referenced row of products and orders, for another transaction inserting the same key, for a lock"
        ),
        "{}",
        f.summary
    );
    assert!(
        f.fix.as_ref().unwrap().summary.contains(
            "Another transaction inserting the same key holds this one until it commits or rolls back, and so does one that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of products and orders"
        ),
        "{}",
        f.fix.as_ref().unwrap().summary
    );
}

// The aptabase-us reading: a stopped insert was told to "find what the slow runs wait for while they happen", and
// Historical outliers do not prove a problem in the replacement. Small recorded I/O doesn't rule storage out.
#[test]
fn a_stopped_unstable_insert_points_at_what_runs_now() {
    let (mean, sd, max, n) = spread_of(&[(2.0, 9_900.0), (5000.0, 100.0)]);
    let mut s = unstable(mean, sd, max, n);
    let q = query(&mut s, "3432126458605143571");
    q.text = "INSERT INTO events (customer_id, kind) VALUES ($1, $2)".into();
    q.plan = Some(insert_plan_into("public.events"));
    // Quiet since the morning before the data ends: a day of silence (four quiet hours are too soon to say it stopped).
    q.first_seen = Some("2026-10-10T06:00:00Z".into());
    q.last_seen = Some("2026-10-10T08:00:00Z".into());
    q.waits = vec![WaitStat {
        category: "IO".into(),
        wait_type: Some("data file reads and writes".into()),
        wait_ms: 86.0,
        tasks: None,
    }];
    let next = query(&mut s, "2885972271926804069");
    next.text = "INSERT INTO events (customer_id, kind, payload) VALUES ($1, $2, $3)".into();
    next.plan = None;
    let report = analyze(s.clone());
    let f = find(&report.findings, "unstable-query:3432126458605143571").expect("an unstable insert");
    assert!(f.inactive, "it stopped");
    let fix = &f.fix.as_ref().unwrap().summary;
    assert!(
        fix.starts_with("This statement no longer runs. Query 2885972271926804069, an insert into events too, still runs and may have replaced it"),
        "{fix}"
    );
    assert!(fix.contains("do not establish a current problem in its replacement or justify a logging change"), "{fix}");
    assert!(!fix.contains("Set log_") && f.fix.as_ref().unwrap().sql.is_none(), "{fix}");
    assert!(f.summary.contains("recorded data-file I/O took 86 ms in all"), "{}", f.summary);
    assert!(f.summary.contains("does not rule out storage delays"), "{}", f.summary);
    assert!(f.summary.contains("Triggers and other work absent from this plan"));
    assert!(f.summary.contains("storage"), "{}", f.summary);
    // Nothing else writes the table: it no longer runs, and that's what it says.
    query(&mut s, "2885972271926804069").text =
        "SELECT kind, count(*) FROM events WHERE customer_id = $1 GROUP BY kind".into();
    let report = analyze(s);
    let fix = report.findings.iter().find(|f| f.check == "unstable-query").unwrap().fix.clone().unwrap().summary;
    assert!(fix.starts_with("This statement no longer runs."), "{fix}");
    assert!(!fix.contains("2885972271926804069") && fix.contains("If slow runs recur"), "{fix}");
}

// SQL Server's Query Store times an autocommit statement's commit with it: its log flush stays among the waits.
#[test]
fn an_unstable_insert_on_sql_server_can_wait_on_its_commit() {
    let mut s = snapshot("mssql-demo.json");
    let (mean, sd, max, n) = spread_of(&[(2.0, 9_900.0), (5000.0, 100.0)]);
    let q = query(&mut s, "6");
    (q.executions, q.mean_time_ms, q.total_time_ms, q.stddev_time_ms, q.max_time_ms) =
        (n, mean, mean * n, Some(sd), Some(max));
    q.text = "INSERT INTO [shop].[products] ([name]) VALUES (@P1)".into();
    let (plan_facts, node_facts) = (
        serde_json::to_value(culprit_core::PlanFacts::default()).unwrap(),
        serde_json::to_value(culprit_core::PlanNodeFacts::default()).unwrap(),
    );
    q.plan = Some(
        serde_json::from_value(serde_json::json!({
            "engine": "mssql", "kind": "estimated", "total_cost": 0.01, "warnings": [], "index_hints": [], "props": {},
            "facts": plan_facts,
            "root": {
                "id": 0, "op": "Clustered Index Insert", "kind": "modify", "object": "[shop].[products]",
                "est_rows": 1.0, "est_executions": 1.0, "subtree_cost": 0.01, "self_cost": 0.01,
                "warnings": [], "props": {}, "facts": node_facts.clone(),
                "children": [{
                    "id": 1, "op": "Constant Scan", "kind": "compute", "est_rows": 1.0, "est_executions": 1.0,
                    "subtree_cost": 0.0, "self_cost": 0.0, "warnings": [], "props": {}, "facts": node_facts,
                    "children": []
                }]
            }
        }))
        .expect("plan"),
    );
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    let report = analyze(s);
    let f = find(&report.findings, "unstable-query:6").expect("an unstable insert");
    assert!(f.summary.contains("the log flush at commit"), "{}", f.summary);
    assert!(!f.summary.contains("FOR KEY SHARE"), "{}", f.summary);
}

/// Statement `id` of `s` with runs that swing: `fast` runs at `fast_ms` and `slow` at `slow_ms`, one plan at most.
fn swinging(mut s: Snapshot, id: &str, (fast, fast_ms): (f64, f64), (slow, slow_ms): (f64, f64)) -> Snapshot {
    let (mean, sd, max, n) = spread_of(&[(fast_ms, fast), (slow_ms, slow)]);
    let q = query(&mut s, id);
    (q.executions, q.mean_time_ms, q.total_time_ms, q.stddev_time_ms, q.max_time_ms) =
        (n, mean, mean * n, Some(sd), Some(max));
    q.plans.truncate(1);
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    s
}

// What unstable-query says, word for word: on PostgreSQL (a read, since a date or over the window; inserts of the
// values they're given, with foreign keys, unique keys, keys skipped, recorded I/O, slow runs too rare to watch, one that
// stopped with and without what replaced it; averages Culprit's scans measured apart), SQL Server (observed waits
// without qualified absence, both blocking and parameter investigations, its commit and plan cache), MySQL (a native
// scalar lock timer, unknown lock time and the slow query log), YugabyteDB and SQLite.
#[test]
fn unstable_queries_say_what_their_slow_runs_waited_for_in_english() {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    assert_eq!(
        crate::info("unstable-query"),
        "Unstable queries\n\
        Queries whose duration swings wildly between runs with the same plan, a sign of parameter sensitivity or blocking."
    );
    const PG: &str = "3432126458605143571";
    let said = |s: Snapshot, id: &str| {
        let findings = crate::run("unstable-query", &s);
        let finding =
            findings.iter().find(|f| f.id == format!("unstable-query:{id}")).expect("recorded unstable runtimes");
        assert_eq!(finding.queries[0].id, id);
        if let Some(sql) = &finding.fix.as_ref().unwrap().sql {
            assert_eq!(
                sql,
                "ALTER SYSTEM SET log_lock_waits = 'on';\nSELECT pg_reload_conf();\nALTER SYSTEM SET log_min_duration_statement = '2s';\nSELECT pg_reload_conf();",
                "only the existing rare-event logging setup is authorized in these cases"
            );
        }
        crate::words(finding)
    };
    let pg = || swinging(snapshot("postgres-demo.json"), PG, (9_900.0, 2.0), (100.0, 5_000.0));
    let mut undated = pg();
    query(&mut undated, PG).first_seen = None;
    let insert = |text: &str, object: &str| {
        let mut s = pg();
        let q = query(&mut s, PG);
        q.text = text.into();
        q.plan = Some(insert_plan_into(object));
        s
    };
    // order_items with a unique key on the order and the product, which the insert sets, and recorded I/O.
    let mut keyed = insert(
        "INSERT INTO order_items (order_id, product_id, quantity) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        "public.order_items",
    );
    let pkey = keyed.indexes.iter().find(|i| i.name == "order_items_pkey").cloned().expect("demo index");
    keyed.indexes.push(culprit_core::IndexStat {
        filter_facts: None,
        name: "order_items_order_product_key".into(),
        columns: vec!["order_id".into(), "product_id".into()],
        is_primary_key: false,
        ..pkey
    });
    let q = query(&mut keyed, PG);
    q.rows = Some(q.executions * 0.25);
    q.waits = vec![WaitStat { category: "IO".into(), wait_type: None, wait_ms: 86.0, tasks: None }];
    let mut rare = keyed.clone();
    rare.workload.as_mut().unwrap().window_hours = 30_000.0;
    let events = || insert("INSERT INTO events (customer_id, kind) VALUES ($1, $2)", "public.events");
    let stopped_insert = |successor: &str| {
        let mut s = events();
        let q = query(&mut s, PG);
        (q.first_seen, q.last_seen) = (Some("2026-10-10T06:00:00Z".into()), Some("2026-10-10T08:00:00Z".into()));
        let next = query(&mut s, "2885972271926804069");
        (next.text, next.plan, next.max_time_ms) = (successor.into(), None, Some(88.0));
        s
    };
    let mut over_scans = measured_by_scans("postgres-demo.json", PG, 50.0, 500.0);
    over_scans = swinging(over_scans, PG, (9_900.0, 2.0), (100.0, 5_000.0));
    query(&mut over_scans, PG).first_seen = over_scans.workload.as_ref().unwrap().window_start.clone();
    let mut totals_only = measured_by_scans("postgres-demo.json", PG, 50.0, 50.0 * 51.98);
    totals_only = swinging(totals_only, PG, (9_900.0, 2.0), (100.0, 5_000.0));
    query(&mut totals_only, PG).first_seen = totals_only.workload.as_ref().unwrap().window_start.clone();
    let mut stopped_read = pg();
    let end = epoch_seconds(stopped_read.workload.as_ref().unwrap().window_end.as_deref().unwrap()).unwrap();
    let q = query(&mut stopped_read, PG);
    (q.first_seen, q.last_seen) =
        (Some(iso_from_epoch(end - 72.0 * 3600.0)), Some(iso_from_epoch(end - 48.0 * 3600.0)));
    let mut actual = Vec::<String>::new();
    actual.extend([
        said(pg(), PG),
        said(undated, PG),
        said(keyed, PG),
        said(rare, PG),
        said(events(), PG),
        said(insert("INSERT INTO app_salts (app_id, salt) VALUES ($1, $2)", "public.app_salts"), PG),
        said(stopped_insert("INSERT INTO events (customer_id, kind, payload) VALUES ($1, $2, $3)"), PG),
        said(stopped_insert("SELECT kind, count(*) FROM events WHERE customer_id = $1 GROUP BY kind"), PG),
        said(over_scans, PG),
        said(totals_only, PG),
        said(stopped_read, PG),
    ]);

    let mssql = |waits: Vec<WaitStat>| {
        let mut s = swinging(snapshot("mssql-demo.json"), "10", (7_990.0, 1.2), (10.0, 1_100.0));
        query(&mut s, "10").waits = waits;
        s
    };
    let wait = |category: &str, wait_type: &str, ms: f64| WaitStat {
        category: category.into(),
        wait_type: Some(wait_type.into()),
        wait_ms: ms,
        tasks: None,
    };
    let mut plan_cache = mssql(Vec::new());
    let w = plan_cache.workload.as_mut().unwrap();
    (w.kind, w.source, w.windowed) = (Some(culprit_core::WorkloadKind::PlanCache), "Plan cache".into(), false);
    let mut commit = swinging(snapshot("mssql-demo.json"), "6", (9_900.0, 2.0), (100.0, 5_000.0));
    let q = query(&mut commit, "6");
    q.text = "INSERT INTO [shop].[order_items] ([order_id], [product_id]) VALUES (@P1, @P2)".into();
    let mut plan = insert_values_plan();
    (plan.engine, plan.root.object) = (culprit_core::Engine::Mssql, Some("[shop].[order_items]".into()));
    q.plan = Some(plan);
    let mut rare_commit = commit.clone();
    rare_commit.workload.as_mut().unwrap().window_hours = 30_000.0;
    actual.extend([
        said(mssql(Vec::new()), "10"),
        said(mssql(vec![wait("Network", "Network IO", 3_406.0), wait("Memory", "Memory", 43.0)]), "10"),
        said(mssql(vec![wait("Lock", "Lock", 10_000.0)]), "10"),
        said(mssql(vec![wait("Lock", "Lock", 1_000.0), wait("IO", "Buffer IO", 2_000.0)]), "10"),
        said(mssql(vec![wait("Memory", "Memory", 900.0)]), "10"),
        said(plan_cache, "10"),
        said(commit, "6"),
        said(rare_commit, "6"),
    ]);

    const MY: &str = "c7b5ac9bb4bcecfe";
    let mysql = |lock_ms: Option<f64>| {
        let mut s = swinging(snapshot("mysql-demo.json"), MY, (1_980.0, 2.0), (20.0, 10_000.0));
        query(&mut s, MY).spread = Some(Spread { lock_ms, ..Default::default() });
        s
    };
    let yugabyte = swinging(snapshot("yugabyte-demo.json"), "8345917977273313509", (1_980.0, 8.0), (20.0, 10_000.0));
    let sqlite = swinging(snapshot("sqlite-demo.json"), "c1b0de997fac4faa", (1_980.0, 8.0), (20.0, 10_000.0));
    actual.extend([
        said(mysql(Some(250_000.0)), MY),
        said(mysql(None), MY),
        said(slow_log(mysql(Some(1_000.0))), MY),
        said(yugabyte, "8345917977273313509"),
        said(sqlite, "c1b0de997fac4faa"),
    ]);

    // One foreign key; a unique key the insert sets, and none.
    actual.extend([
        said(insert("INSERT INTO orders (customer_id, status) VALUES ($1, $2)", "public.orders"), PG),
        said(insert("INSERT INTO products (sku, name) VALUES ($1, $2)", "public.products"), PG),
    ]);

    // SQL Server: an insert whose one foreign key checks each row, and waits Query Store recorded on CPU alone.
    let mut one_key = swinging(snapshot("mssql-demo.json"), "6", (9_900.0, 2.0), (100.0, 5_000.0));
    let q = query(&mut one_key, "6");
    q.text = "INSERT INTO [shop].[orders] ([customer_id], [status]) VALUES (@P1, @P2)".into();
    let mut plan = insert_values_plan();
    (plan.engine, plan.root.object) = (culprit_core::Engine::Mssql, Some("[shop].[orders]".into()));
    q.plan = Some(plan);
    actual.extend([said(one_key, "6"), said(mssql(vec![wait("CPU", "CPU", 500.0)]), "10")]);

    const PINNED: &str = r###"A read from customers averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 11, 07:01 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell which values were slow or how many calls were.

A read from customers averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement in the 5 hours since statistics were reset, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell which values were slow or how many calls were.

An insert into order_items averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Its plan shows no read, but each run looks its key up in order_items_order_product_key to see whether a row already has it, and its foreign keys on product_id and order_id check each row it inserts against products and orders: the referenced row is looked up and share-locked (FOR KEY SHARE). 75% of its runs found their key already there and inserted nothing. Its recorded data-file I/O took 86 ms in all, against at least 8 min of extra time in the slow runs. I/O timing may not cover every historical run, so this does not rule out storage delays. The slow runs may have been waiting for a transaction that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of products and orders, for another transaction inserting the same key, for a lock on the table (a migration's, say), or on storage (writing WAL or data pages while a checkpoint runs). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
metric: Data-file reads and writes
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). Another transaction inserting the same key holds this one until it commits or rolls back, and so does one that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of products and orders: keep those transactions short. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 11, 07:01 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell how many calls were slow.

An insert into order_items averages 52 ms per run, but some runs take far longer (up to 5.0 s) · Low · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Its plan shows no read, but each run looks its key up in order_items_order_product_key to see whether a row already has it, and its foreign keys on product_id and order_id check each row it inserts against products and orders: the referenced row is looked up and share-locked (FOR KEY SHARE). 75% of its runs found their key already there and inserted nothing. Its recorded data-file I/O took 86 ms in all, against at least 8 min of extra time in the slow runs. I/O timing may not cover every historical run, so this does not rule out storage delays. The slow runs may have been waiting for a transaction that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of products and orders, for another transaction inserting the same key, for a lock on the table (a migration's, say), or on storage (writing WAL or data pages while a checkpoint runs). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
metric: Data-file reads and writes
fix: The extra time can come from as few as 100 slow runs in the 1250 days since statistics were reset, too few to count on catching one while it happens. Have the next ones logged instead. Set log_lock_waits to on and log_min_duration_statement to 2s and reload the configuration (settings marked as needing a restart take effect after one). log_lock_waits then logs each lock wait longer than deadlock_timeout (1 s by default) with the process holding the lock, and log_min_duration_statement each statement slower than 2s (any statement, not only this one) with its duration. Another transaction inserting the same key holds this one until it commits or rolls back, and so does one that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of products and orders: keep those transactions short. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
sql: ALTER SYSTEM SET log_lock_waits = 'on';
SELECT pg_reload_conf();
ALTER SYSTEM SET log_min_duration_statement = '2s';
SELECT pg_reload_conf();
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 11, 07:01 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell how many calls were slow.

An insert into events averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Its plan reads no table (it writes the values it is given), the captured plan does not show all work in triggers, constraints or called functions. The slow runs may have been waiting for a lock on the table (a migration's, say), or on storage (writing WAL or data pages while a checkpoint runs). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). A transaction that locks the table (a migration's, say) holds this one until it commits or rolls back: keep those transactions short. Compare storage timing during a slow run with its other work. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 11, 07:01 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell how many calls were slow.

An insert into app_salts averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Its plan reads no table (it writes the values it is given), the captured plan does not show all work in triggers, constraints or called functions. The slow runs may have been waiting for another transaction inserting the same key, for a lock on the table (a migration's, say), or on storage (writing WAL or data pages while a checkpoint runs). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). Another transaction inserting the same key holds this one until it commits or rolls back: keep those transactions short. Compare storage timing during a slow run with its other work. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 11, 07:01 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell how many calls were slow.

An insert into events averaged 52 ms per run, but some runs took far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Its plan reads no table (it writes the values it is given), the captured plan does not show all work in triggers, constraints or called functions. The slow runs may have been waiting for a lock on the table (a migration's, say), or on storage (writing WAL or data pages while a checkpoint runs). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: This statement no longer runs. Query 2885972271926804069, an insert into events too, still runs and may have replaced it. Its slowest recorded run took 88 ms. These historical outliers do not establish a current problem in its replacement or justify a logging change. If slow runs recur, collect their durations and waits and inspect trigger and constraint work before choosing a fix.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 10, 06:00 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell how many calls were slow.

An insert into events averaged 52 ms per run, but some runs took far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Its plan reads no table (it writes the values it is given), the captured plan does not show all work in triggers, constraints or called functions. The slow runs may have been waiting for a lock on the table (a migration's, say), or on storage (writing WAL or data pages while a checkpoint runs). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: This statement no longer runs. These historical outliers do not establish a current problem in its replacement or justify a logging change. If slow runs recur, collect their durations and waits and inspect trigger and constraint work before choosing a fix.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 10, 06:00 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell how many calls were slow.

A read from customers averages 10 ms per run, but some runs take far longer (up to 5.0 s) · Low · Medium
Same statement, very different run times: the slow runs took at least 8 min more, together, than its average of 52 ms a run over the 40 days since statistics were reset (10 ms over 36 hours of Culprit's scans). Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 73% of DB time in slow runs
metric: Average over Culprit's scans
metric: Average over the 40 days since statistics were reset
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time over the 40 days since statistics were reset (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Sep 1, 11:54 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell which values were slow or how many calls were.

A read from customers averages 52 ms per run, but some runs take far longer (up to 5.0 s) · Low · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together over the 40 days since statistics were reset. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 73% of DB time in slow runs
metric: Average over Culprit's scans
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time over the 40 days since statistics were reset (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Sep 1, 11:54 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell which values were slow or how many calls were.

A read from customers averaged 52 ms per run, but some runs took far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 8, 11:54 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell which values were slow or how many calls were.

A read from support_tickets averages 2.6 ms per run, but some runs take far longer (up to 1.1 s) · Low · Medium
Same query, same plan, very different run times: the slow runs took at least 10.9 s more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 20% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

A read from support_tickets averages 2.6 ms per run, but some runs take far longer (up to 1.1 s) · Low · Medium
Same query, same plan, very different run times: the slow runs took at least 10.9 s more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 20% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

A read from support_tickets averages 2.6 ms per run, but some runs take far longer (up to 1.1 s) · Low · Medium
Same query, same plan, very different run times: the slow runs took at least 10.9 s more than the average, together. At least 10.0 s of lock waits were recorded by Query Store, against at least 10.9 s of extra time in the slow runs; blocking is a candidate, but these totals do not identify which calls waited or rule out parameter-sensitive plans. Compare the parameter values of the slow runs with those of the fast ones.
impact: 20% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help. Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

A read from support_tickets averages 2.6 ms per run, but some runs take far longer (up to 1.1 s) · Low · Medium
Same query, same plan, very different run times: the slow runs took at least 10.9 s more than the average, together. At least 1.0 s of lock waits were recorded by Query Store, against at least 10.9 s of extra time in the slow runs; blocking is a candidate, but these totals do not identify which calls waited or rule out parameter-sensitive plans. Compare the parameter values of the slow runs with those of the fast ones.
impact: 20% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help. Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

A read from support_tickets averages 2.6 ms per run, but some runs take far longer (up to 1.1 s) · Low · Medium
Same query, same plan, very different run times: the slow runs took at least 10.9 s more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 20% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

A read from support_tickets averages 2.6 ms per run, but some runs take far longer (up to 1.1 s) · Low · Medium
Same query, same plan, very different run times: the slow runs took at least 10.9 s more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 20% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: On a test copy, capture actual execution plans and duration samples for representative parameter values before and after the change, under comparable load. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. Plan cache keeps running totals, so its cached maximum and standard deviation do not isolate runs after the change; compare the new execution samples instead.

An insert into order_items averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same query, same plan, very different run times: the slow runs took at least 8 min more than the average, together. Its plan reads no table (it writes the values it is given), but its foreign keys on order_id and product_id check each row it inserts against orders and products: the referenced row is looked up and share-locked. The slow runs may have been waiting for a transaction that locks, updates or deletes the referenced row of orders and products, for a lock on the table (a migration's, say), or on storage (the log flush at commit, when it commits on its own, or a checkpoint). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 88% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). A transaction that locks, updates or deletes the referenced row of orders and products holds this one until it commits or rolls back: keep those transactions short. Compare log and storage timing during a slow run with its other work. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

An insert into order_items averages 52 ms per run, but some runs take far longer (up to 5.0 s) · Medium · Medium
Same query, same plan, very different run times: the slow runs took at least 8 min more than the average, together. Its plan reads no table (it writes the values it is given), but its foreign keys on order_id and product_id check each row it inserts against orders and products: the referenced row is looked up and share-locked. The slow runs may have been waiting for a transaction that locks, updates or deletes the referenced row of orders and products, for a lock on the table (a migration's, say), or on storage (the log flush at commit, when it commits on its own, or a checkpoint). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 88% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: The extra time can come from as few as 100 slow runs in the last 1250 days, too few to count on catching one while it happens. Query Store keeps this query's waits for each interval (sys.query_store_wait_stats): the intervals of its slowest runs say what they waited on. A transaction that locks, updates or deletes the referenced row of orders and products holds this one until it commits or rolls back: keep those transactions short. Compare log and storage timing during a slow run with its other work. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

A read from orders averages 102 ms per run, but some runs take far longer (up to 10.0 s) · Medium · Medium
Same statement, very different run times: the slow runs took at least 3 min more than the average, together. At least 4 min of lock waits were recorded by MySQL, against at least 3 min of extra time in the slow runs; blocking is a candidate, but these totals do not identify which calls waited or rule out parameter-sensitive plans. Compare the parameter values of the slow runs with those of the fast ones.
impact: 81% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help. Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Capture slow runs in the slow query log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. performance_schema keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.

A read from orders averages 102 ms per run, but some runs take far longer (up to 10.0 s) · Medium · Medium
Same statement, very different run times: the slow runs took at least 3 min more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 81% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Capture slow runs in the slow query log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. performance_schema keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.

A read from orders averages 102 ms per run, but some runs take far longer (up to 10.0 s) · Medium · Medium
Same statement, very different run times: the slow runs took at least 3 min more than the average, together. At least 1.0 s of lock waits were recorded by MySQL, against at least 3 min of extra time in the slow runs; blocking is a candidate, but these totals do not identify which calls waited or rule out parameter-sensitive plans. Compare the parameter values of the slow runs with those of the fast ones.
impact: 81% of logged-run time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help. Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Collect equal-length query-log samples before and after the change, keeping the same logging threshold and a representative workload. Include only runs whose timestamps fall in their respective period. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

A read from orders averages 108 ms per run, but some runs take far longer (up to 10.0 s) · Medium · Medium
Same statement, very different run times: the slow runs took at least 3 min more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 91% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. YSQL pg_stat_statements (node 3d2d9e10555b4ae2a683a8402505e416) keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: YugabyteDB keeps one minimum, average, maximum and standard deviation per statement in the 1 minute since statistics were reset, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell which values were slow or how many calls were.

A read from orders averages 108 ms per run, but some runs take far longer (up to 10.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 3 min more than the average, together. Either some values match far more rows than the plan expects, or some runs wait for the file's write lock (up to the application's busy_timeout). Compare the slow runs' values in the log.
impact: 83% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Collect equal-length query-log samples before and after the change, keeping the same logging threshold and a representative workload. Include only runs whose timestamps fall in their respective period. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

An insert into orders averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Its plan reads no table (it writes the values it is given), but its foreign key on customer_id checks each row it inserts against customers: the referenced row is looked up and share-locked (FOR KEY SHARE). The slow runs may have been waiting for a transaction that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of customers, for a lock on the table (a migration's, say), or on storage (writing WAL or data pages while a checkpoint runs). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). A transaction that locks (SELECT … FOR UPDATE), deletes or changes the key of the referenced row of customers holds this one until it commits or rolls back: keep those transactions short. Compare storage timing during a slow run with its other work. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 11, 07:01 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell how many calls were slow.

An insert into products averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same statement, very different run times: the slow runs took at least 8 min more than the average, together. Its plan shows no read, but each run looks its key up in products_sku_key to see whether a row already has it: one lookup whatever the values, the captured plan does not show all work in triggers, constraints or called functions. The slow runs may have been waiting for another transaction inserting the same key, for a lock on the table (a migration's, say), or on storage (writing WAL or data pages while a checkpoint runs). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 73% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). Another transaction inserting the same key holds this one until it commits or rolls back: keep those transactions short. Compare storage timing during a slow run with its other work. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
verify: Capture slow runs in the server log before the change and again after it, with the same logging threshold and a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent. pg_stat_statements keeps running totals, so its historical maximum and standard deviation do not isolate runs after the change; use the new log samples to judge it.
caveat: PostgreSQL keeps one minimum, average, maximum and standard deviation per statement since Oct 11, 07:01 UTC, so the slowest runs could be from any time in that span (a lock wait during a migration counts too), and Culprit can't tell how many calls were slow.

An insert into orders averages 52 ms per run, but some runs take far longer (up to 5.0 s) · High · Medium
Same query, same plan, very different run times: the slow runs took at least 8 min more than the average, together. Its plan reads no table (it writes the values it is given), but its foreign key on customer_id checks each row it inserts against customers: the referenced row is looked up and share-locked. The slow runs may have been waiting for a transaction that locks, updates or deletes the referenced row of customers, for a lock on the table (a migration's, say), or on storage (the log flush at commit, when it commits on its own, or a checkpoint). Triggers and other work absent from this plan can also vary between runs; measure a slow run before choosing a cause.
impact: 88% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Find what the slow runs wait for while they happen (Live shows each session's wait and any blocking chain). A transaction that locks, updates or deletes the referenced row of customers holds this one until it commits or rolls back: keep those transactions short. Compare log and storage timing during a slow run with its other work. Inspect the blocking transaction and any trigger or constraint work before deciding whether a query, index or transaction change will help.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent.

A read from support_tickets averages 2.6 ms per run, but some runs take far longer (up to 1.1 s) · Low · Medium
Same query, same plan, very different run times: the slow runs took at least 10.9 s more than the average, together. Wait coverage is incomplete, so the observations cannot rule out locks or establish which runs were slow. Compare the parameter values of the slow runs with those of the fast ones.
impact: 20% of DB time in slow runs
metric: Average
metric: Slowest
metric: Std deviation
metric: Slow runs' extra time (at least)
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: Compare equal Query Store windows before and after the change, with the later window containing only runs after it. Check the query's average and slowest run times, and lock waits per run where recorded, under a representative workload. Compare their durations and any recorded lock waits: runs with comparable parameter values should be faster; if locks caused the slow runs, those waits should be shorter or absent."###;
    crate::pins::pin_text("unstable-query-words", &actual.join("\n\n"), PINNED);
}

// ── stopped running ──────────────────────────────────────────────────────────────────────────────

// The aptabase-us reading: pg_stat_statements keeps no last run, so Culprit dates one by its own scans, and a
// statement that hasn't grown since the first of them last ran by then at the latest. "Has not run since" that scan
// said it stopped then, and "a deploy most likely changed it" sent the reader to that day's deploy.
#[test]
fn a_statement_quiet_since_the_first_scan_may_have_stopped_earlier() {
    let mut s = snapshot("postgres-demo.json");
    s.workload.as_mut().unwrap().timeline = Some(
        serde_json::from_value(serde_json::json!({
            "source": "scans",
            "starts": ["2026-10-11T08:00:00Z", "2026-10-11T09:00:00Z", "2026-10-11T10:00:00Z", "2026-10-11T11:00:00Z"],
            "end": "2026-10-11T11:54:21Z",
            "time_ms": [1000.0, 1000.0, 1000.0, 1000.0],
            "executions": [100.0, 100.0, 100.0, 100.0]
        }))
        .expect("timeline"),
    );
    let q = query(&mut s, ORDERS_SORT);
    q.trend = None;
    q.last_seen = Some("2026-10-11T08:00:00Z".into());
    // Four hours of Culprit's scans without a run: too soon to say it stopped.
    let report = analyze(s.clone());
    let f = find(&report.findings, &format!("temp-spill:{ORDERS_SORT}")).expect("the spill");
    assert!(!f.inactive);
    assert_eq!(
        f.caveats[0],
        "This query hasn't run in the 3 h 54 min since Culprit's first scan (Oct 11, 08:00 UTC): too short a time to say it stopped."
    );
    // A day and more of them.
    let timeline = s.workload.as_mut().unwrap().timeline.as_mut().unwrap();
    timeline.starts = ["2026-10-10T08:00:00Z", "2026-10-10T16:00:00Z", "2026-10-11T00:00:00Z", "2026-10-11T08:00:00Z"]
        .map(String::from)
        .to_vec();
    query(&mut s, ORDERS_SORT).last_seen = Some("2026-10-10T08:00:00Z".into());
    let report = analyze(s.clone());
    let f = find(&report.findings, &format!("temp-spill:{ORDERS_SORT}")).expect("the spill");
    assert!(f.inactive);
    assert!(
        f.caveats[0].starts_with("This query hasn't run in the ")
            && f.caveats[0].contains(" since Culprit's first scan (Oct 10, 08:00 UTC); it may have stopped earlier."),
        "{}",
        f.caveats[0]
    );
    assert!(!f.caveats[0].contains("most likely"), "{}", f.caveats[0]);
    // Seen growing at a later scan: it ran until about then, and a deploy most likely changed it.
    query(&mut s, ORDERS_SORT).last_seen = Some("2026-10-10T09:00:00Z".into());
    let report = analyze(s);
    let f = find(&report.findings, &format!("temp-spill:{ORDERS_SORT}")).expect("the spill");
    assert!(f.caveats[0].starts_with("This query has not run since 2026-10-10 09:00"), "{}", f.caveats[0]);
}

// ── temp-spill ───────────────────────────────────────────────────────────────────────────────────

#[test]
fn temporary_source_metadata_does_not_complete_an_unmeasured_check() {
    let mut s = snapshot("postgres-demo.json");
    for q in &mut s.workload.as_mut().unwrap().queries {
        q.temporary_work = Some(culprit_core::StatementTemporaryWork::BlockIo {
            source: "native".into(),
            bytes_per_block: Some(8192),
            read_blocks: None,
            written_blocks: None,
            read_time_ms: None,
            write_time_ms: None,
        });
    }
    let report = analyze(s.clone());
    let assessment = report.assessments.iter().find(|assessment| assessment.check == "temp-spill").unwrap();
    assert_eq!(assessment.status, culprit_core::CheckStatus::InsufficientEvidence);
    assert!(assessment.assessed_queries.is_empty());
    for q in &mut s.workload.as_mut().unwrap().queries {
        if let Some(culprit_core::StatementTemporaryWork::BlockIo { written_blocks, .. }) = &mut q.temporary_work {
            *written_blocks = Some("0".into());
        }
    }
    let report = analyze(s);
    assert_eq!(
        report.assessments.iter().find(|assessment| assessment.check == "temp-spill").unwrap().status,
        culprit_core::CheckStatus::Assessed
    );
}

const ORDERS_SORT: &str = "9065391607317686058";

#[test]
fn temporary_work_does_not_estimate_delay_from_an_assumed_disk_speed() {
    let mut s = snapshot("postgres-demo.json");
    let q = query(&mut s, ORDERS_SORT);
    q.temporary_work = Some(culprit_core::StatementTemporaryWork::BlockIo {
        source: "pg_stat_statements".into(),
        bytes_per_block: Some(8192),
        read_blocks: None,
        written_blocks: Some(format!("{:.0}", 2.0 * q.executions)),
        read_time_ms: None,
        write_time_ms: None,
    });
    let report = analyze(s);
    let f = find(&report.findings, &format!("temp-spill:{ORDERS_SORT}")).unwrap();
    assert_eq!(f.actionability, culprit_core::Actionability::Information);
    assert_eq!(f.severity, Severity::Low);
    assert!(f.impact.db_time_share.is_none() && f.impact.time_ms_per_day.is_none());
    assert!(f.fix.as_ref().unwrap().sql.is_none());
}

#[test]
fn temporary_totals_do_not_size_application_memory_from_average_written_bytes() {
    for limit in ["4MB", "64MB", "1GB"] {
        let mut s = snapshot("postgres-demo.json");
        set(&mut s, "work_mem", limit);
        let report = analyze(s);
        let f = find(&report.findings, &format!("temp-spill:{ORDERS_SORT}")).unwrap();
        assert!(f.fix.as_ref().unwrap().sql.is_none());
        assert_eq!(f.confidence, culprit_core::Confidence::High);
        assert_eq!(f.actionability, culprit_core::Actionability::Information);
        assert!(f.caveats.iter().any(|text| text.contains("without establishing memory exhaustion")));
    }
}

#[test]
fn cumulative_temp_work_does_not_blame_a_plans_only_capable_operator() {
    for (name, id) in [("postgres-demo.json", ORDERS_SORT), ("mysql-demo.json", GROUP_BY)] {
        let report = analyze(snapshot(name));
        let f = find(&report.findings, &format!("temp-spill:{id}")).unwrap();
        assert_eq!(f.queries[0].plan_node, None);
        assert_eq!(f.facts.cause, None);
        assert!(f.impact.db_time_share.is_none());
        assert!(f.fix.as_ref().unwrap().sql.is_none());
    }
}

#[test]
fn an_exchange_is_never_a_plans_costliest_step() {
    // The demo's Gather Merge is costed above the scan for passing 667K rows between processes: not work on the data.
    let s = snapshot("postgres-demo.json");
    let plan = s.workload.as_ref().unwrap().query(ORDERS_SORT).and_then(|q| q.plan.as_ref()).expect("plan");
    assert_eq!(plan.root.op, "Gather Merge");
    assert!(plan.root.self_cost > plan.nodes().iter().filter(|n| n.id != 0).map(|n| n.self_cost).fold(0.0, f64::max));
    assert_eq!(plan.hottest().map(|n| n.op.as_str()), Some("Seq Scan"));
}

const GROUP_BY: &str = "04cd7be9c366fadc";

#[test]
fn native_disk_table_counts_do_not_prove_memory_exhaustion_or_a_required_limit_change() {
    for engine in ["TempTable", "MEMORY"] {
        let mut s = snapshot("mysql-demo.json");
        set(&mut s, "internal_tmp_mem_storage_engine", engine);
        set(&mut s, "big_tables", "ON");
        let report = analyze(s);
        let f = find(&report.findings, &format!("temp-spill:{GROUP_BY}")).unwrap();
        assert!(f.fix.as_ref().unwrap().sql.is_none());
        assert!(f.impact.db_time_share.is_none());
        assert!(f.caveats.iter().any(|text| text.contains("Disk tables can be created directly")));
    }
}

#[test]
fn native_table_and_sort_pass_counts_preserve_exact_values_without_time_allocation() {
    let mut s = snapshot("mysql-demo.json");
    query(&mut s, GROUP_BY).temporary_work = Some(culprit_core::StatementTemporaryWork::TableCreation {
        source: "performance_schema.events_statements_summary_by_digest".into(),
        created_tables: Some("18446744073709551615".into()),
        disk_tables: Some("9007199254740993".into()),
        sort_merge_passes: Some("0".into()),
    });
    let report = analyze(s);
    let f = find(&report.findings, &format!("temp-spill:{GROUP_BY}")).unwrap();
    let values: Vec<_> = f.metrics.iter().filter_map(|metric| metric.text.as_deref()).collect();
    assert!(values.contains(&"18446744073709551615") && values.contains(&"9007199254740993") && values.contains(&"0"));
    assert!(f.impact.db_time_share.is_none() && f.impact.time_ms_per_day.is_none());
}

/// `s` with statement `id` stopped: it ran for a day, two days before the data ends.
fn stopped(mut s: Snapshot, id: &str) -> Snapshot {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    let end = s.workload.as_ref().and_then(|w| w.window_end.as_deref()).and_then(epoch_seconds).expect("an end");
    let q = query(&mut s, id);
    (q.first_seen, q.last_seen) =
        (Some(iso_from_epoch(end - 72.0 * 3600.0)), Some(iso_from_epoch(end - 48.0 * 3600.0)));
    s
}

/// The demo `name` over 40 days of running totals, with three of Culprit's scans in its last 36 hours that measured
/// statement `id` at `runs` and `time_ms` in each.
fn measured_by_scans(name: &str, id: &str, runs: f64, time_ms: f64) -> Snapshot {
    let mut s = snapshot(name);
    let w = s.workload.as_mut().unwrap();
    let end = w.window_end.clone().expect("an end");
    let start = culprit_core::format::epoch_seconds(&end).unwrap() - 40.0 * 86_400.0;
    (w.window_start, w.window_hours) = (Some(culprit_core::format::iso_from_epoch(start)), 40.0 * 24.0);
    let at = |hours: f64| {
        culprit_core::format::iso_from_epoch(culprit_core::format::epoch_seconds(&end).unwrap() - hours * 3600.0)
    };
    w.timeline = Some(culprit_core::Timeline {
        source: culprit_core::TimelineSource::Scans,
        starts: vec![at(36.0), at(24.0), at(12.0)],
        end: end.clone(),
        time_ms: vec![10_000.0; 3],
        executions: vec![5_000.0; 3],
        unknown: None,
    });
    query(&mut s, id).trend =
        Some(culprit_core::QueryTrend { time_ms: vec![Some(time_ms); 3], executions: vec![Some(runs); 3] });
    s
}

// Native resource observations retain their own meaning without sizing memory or assigning elapsed cost.
#[test]
fn temporary_work_states_its_native_meaning_and_unknown_cause_in_english() {
    assert_eq!(
        crate::info("temp-spill"),
        "Temporary resource work\nRecorded temporary I/O, internal table creation and space use, without inferring elapsed cost or a memory shortage."
    );
    let report = analyze(snapshot("postgres-demo.json"));
    let f = find(&report.findings, &format!("temp-spill:{ORDERS_SORT}")).unwrap();
    assert_eq!(
        f.summary,
        "The source recorded temporary block traffic and any available I/O timings for this statement. Individual executions and operators remain unresolved."
    );
    let report = analyze(snapshot("mysql-demo.json"));
    let f = find(&report.findings, &format!("temp-spill:{GROUP_BY}")).unwrap();
    assert_eq!(
        f.summary,
        "The source recorded internal temporary-table and sort-pass counts for this statement. Their byte volume and elapsed cost remain unknown."
    );
    let history = analyze(stopped(snapshot("postgres-demo.json"), ORDERS_SORT));
    assert_eq!(
        find(&history.findings, &format!("temp-spill:{ORDERS_SORT}")).unwrap().actionability,
        culprit_core::Actionability::History
    );
    let compared = analyze(measured_by_scans("postgres-demo.json", ORDERS_SORT, 10.0, 2000.0));
    let f = find(&compared.findings, &format!("temp-spill:{ORDERS_SORT}")).unwrap();
    assert!(f.caveats.iter().any(|text| text.starts_with("These resource totals cover ")));
    assert!(f.impact.db_time_share.is_none());
}

// ── heavy-query ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn an_explicit_sleep_is_named_but_low_read_updates_have_no_established_wait_cause() {
    let report = analyze(snapshot("postgres-demo.json"));
    let sleep = find(&report.findings, "heavy-query:-8886085649470066503").unwrap();
    assert!(sleep.title.contains("of database time over") && sleep.title.ends_with(", sleeping"), "{}", sleep.title);
    assert!(!sleep.fix.as_ref().unwrap().summary.contains("reads more rows"));
    let update = find(&report.findings, "heavy-query:4911431806993420100").unwrap();
    assert!(update.title.contains("19.0 s of database time"), "{}", update.title);
    assert!(!update.title.contains("waiting") && !update.title.contains("locks"), "{}", update.title);
    assert!(
        !update.summary.contains("most likely") && !update.summary.contains("another transaction held"),
        "{}",
        update.summary
    );
    assert!(update.metrics.iter().any(|m| m.label == "Rows changed per run"));
}

#[test]
fn query_store_waits_name_what_a_statement_waited_for() {
    // Lock and network waits have findings of their own (lock-time, network-wait), which then explain the statement;
    // latch waits are this one's to name.
    let mut s = snapshot("mssql-demo.json");
    let q = query(&mut s, "10");
    q.mean_time_ms = 900.0;
    q.total_time_ms = 900.0 * q.executions;
    q.cpu_time_ms = Some(q.total_time_ms * 0.02);
    q.waits = vec![WaitStat {
        category: "Latch".into(),
        wait_type: Some("Buffer Latch".into()),
        wait_ms: q.total_time_ms * 0.9,
        tasks: None,
    }];
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    let report = analyze(s);
    let named: Vec<String> = report
        .findings
        .iter()
        .filter(|f| f.queries.iter().any(|q| q.id == "10"))
        .map(|f| format!("{} | {}", f.id, f.title))
        .collect();
    let f = find(&report.findings, "heavy-query:10").unwrap_or_else(|| panic!("{named:#?}"));
    assert!(f.title.ends_with("with at least 54 min of recorded waiting (Buffer Latch)"), "{}", f.title);
    assert!(
        f.summary.contains("Query Store recorded at least 54 min of waiting (Buffer Latch)")
            && f.summary.contains("measured CPU time is"),
        "{}",
        f.summary
    );
    assert!(!f.summary.contains("2%") && !f.summary.contains("other sessions are using"), "{}", f.summary);
}

// The fresh SQL Server demo: 750 runs in three minutes, then none, read as "about 123 runs a day"; the fresh PostgreSQL
// demo's three runs in six days as "about 0.5 times a day".
#[test]
fn a_heavy_query_says_when_a_burst_ran_and_counts_a_few_runs() {
    let mut s = point_lookup(1.0, 0.0066);
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-10-05T12:13:47Z".into()), 144.0);
    let q = query(&mut s, "7");
    (q.first_seen, q.last_seen) = (Some("2026-10-11T07:44:33Z".into()), Some("2026-10-11T07:47:52Z".into()));
    let f = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy");
    assert!(f.summary.contains("It ran 3.0K times at 1 ms on average, on Oct 11, 07:44–07:47 UTC: "), "{}", f.summary);
    assert!(!f.summary.contains("a day"), "{}", f.summary);
    assert!(!f.metrics.iter().any(|m| m.label == "Runs per day"), "{:?}", f.metrics);
    assert!(f.metrics.iter().any(|m| m.label == "Runs" && m.value == 3_000.0), "{:?}", f.metrics);

    let mut s = snapshot("postgres-demo.json");
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-10-05T11:54:21Z".into()), 144.0);
    let q = query(&mut s, "4911431806993420100");
    (q.executions, q.total_time_ms) = (3.0, 3.0 * q.mean_time_ms);
    let f = find(&analyze(s).findings, "heavy-query:4911431806993420100").cloned().expect("heavy");
    assert!(f.summary.contains("It ran 3 times at 19.0 s on average: "), "{}", f.summary);
}

/// The PostgreSQL demo as if pg_stat_statements had counted for 40 days, with 36 hours of Culprit's own scans in three
/// steps of 10 s of database time each, in which statement `id` ran `runs` times in `time_ms` a step.
fn with_scans(id: &str, runs: f64, time_ms: f64) -> Snapshot {
    let mut s = snapshot("postgres-demo.json");
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-09-01T11:54:21Z".into()), 40.0 * 24.0);
    w.timeline = Some(
        serde_json::from_value(serde_json::json!({
            "source": "scans",
            "starts": ["2026-10-10T00:00:00Z", "2026-10-10T12:00:00Z", "2026-10-11T00:00:00Z"],
            "end": "2026-10-11T12:00:00Z",
            "time_ms": [10000.0, 10000.0, 10000.0],
            "executions": [5000.0, 5000.0, 5000.0]
        }))
        .expect("timeline"),
    );
    query(&mut s, id).trend =
        Some(culprit_core::QueryTrend { time_ms: vec![Some(time_ms); 3], executions: vec![Some(runs); 3] });
    s
}

// The aptabase readings: "about 388 times a day" and 31% averaged over three and a half years, where Culprit's own scans
// measured 1,670 a day and 43%; and "re-scan and compare this query's share", which a week moves by a hair and which a
// rewritten statement (a new id) freezes.
#[test]
fn a_heavy_query_on_running_totals_says_what_culprits_scans_measured() {
    // Forty runs in 40 days, 15 of them in the 36 hours of scans: 10 a day, most of the time there.
    let mut s = with_scans("4911431806993420100", 5.0, 9_500.0);
    let q = query(&mut s, "4911431806993420100");
    (q.executions, q.total_time_ms, q.logical_reads) = (40.0, 40.0 * q.mean_time_ms, Some(40.0 * 15.0));
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    let report = analyze(s);
    let f = find(&report.findings, "heavy-query:4911431806993420100").expect("heavy");
    // The scans' figures lead, the totals since the reset beside them where they differ twofold.
    assert!(
        f.summary.contains(
            "It ran about 10 times a day at 1.9 s on average over 36 hours of Culprit's own scans: 95% of all database time in them ("
        ) && f.summary.contains("about 1 run a day and 19.0 s on average over the 40 days since statistics were reset)."),
        "{}",
        f.summary
    );
    assert!(f.title.contains("28.5 s of database time over 36 hours of Culprit's scans (95%)"), "{}", f.title);
    assert!(f.metrics.iter().any(|m| m.label == "Runs per day over Culprit's scans"), "{:?}", f.metrics);
    let metric = |label: &str| f.metrics.iter().find(|m| m.label == label).map(|m| m.value);
    assert_eq!(metric("Average duration over Culprit's scans"), Some(1_900.0));
    assert!(metric("Average duration over the 40 days since statistics were reset").is_some(), "{:?}", f.metrics);
    let verify = &f.verify[0];
    assert!(
        verify.starts_with("pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change.")
            && verify.contains("Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there ")
            && verify.ends_with("A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."),
        "{verify}"
    );
}

// The aptabase-us reading: the insert that replaced a stopped one took 12% of database time over Culprit's scans, but
// its entry was days old, so its share since 2023 was 0.13% and it had no finding while the stopped one had two.
#[test]
fn a_recent_entry_is_judged_by_culprits_scans() {
    // 6K of its 8.2K runs came in the scans, 1 s of every 10 s step.
    let report = analyze(with_scans("3432126458605143571", 2_000.0, 1_000.0));
    let f = find(&report.findings, "heavy-query:3432126458605143571").expect("heavy now");
    assert!(f.title.ends_with("over 36 hours of Culprit's scans (10%)"), "{}", f.title);
    assert!(
        f.summary.contains(
            "Its entry in pg_stat_statements is recent: 6.0K of its 8.2K runs came during 36 hours of Culprit's own scans, where it ran about 4.0K times a day at 0.50 ms on average and took 10% of database time."
        ),
        "{}",
        f.summary
    );
    assert!((f.impact.db_time_share.unwrap() - 0.1).abs() < 1e-9, "{:?}", f.impact);
    // Under a day of scans, its share of the totals decides, and it isn't heavy.
    let mut s = with_scans("3432126458605143571", 2_000.0, 1_000.0);
    let w = s.workload.as_mut().unwrap();
    let t = w.timeline.as_mut().unwrap();
    t.starts = vec!["2026-10-11T00:00:00Z".into(), "2026-10-11T04:00:00Z".into(), "2026-10-11T08:00:00Z".into()];
    assert!(find(&analyze(s).findings, "heavy-query:3432126458605143571").is_none());
    // An old entry whose runs mostly came before the scans is read by them too, in both directions: 10% over them,
    // against well under 1% of the totals since the reset, said beside them.
    let report = analyze(with_scans("3432126458605143571", 100.0, 1_000.0));
    let f = find(&report.findings, "heavy-query:3432126458605143571").expect("heavy over the scans");
    assert!(f.title.ends_with("over 36 hours of Culprit's scans (10%)"), "{}", f.title);
    assert!(
        f.summary
            .contains("over 36 hours of Culprit's own scans: 10% of all database time in them (<1% of database time"),
        "{}",
        f.summary
    );
    assert!(!f.summary.contains("is recent"), "{}", f.summary);
}

// Theme A's open ends, the aptabase-eu reading: chatty's "Runs per day: 43K" beside a title over Culprit's scans and a
// summary that named no period; an unstable query's average over the scans beside its slow runs' excess since 2023.
// Each figure says what it is over: the scans' runs, share and average so, the totals' beside them where they differ
// twofold, and what only the totals keep (rows a run, the slow runs' excess) over those.
#[test]
fn figures_over_the_scans_and_over_the_totals_each_say_what_they_are_over() {
    let totals = "over the 40 days since statistics were reset";
    // 15K of a 12-hour step's 50K runs (30%, 30K a day over the scans); 4M since the reset (100K a day, 5% of 80M).
    let mut s = with_scans("3432126458605143571", 15_000.0, 7_500.0);
    let w = s.workload.as_mut().unwrap();
    let t = w.timeline.as_mut().unwrap();
    (t.executions, t.time_ms) = (vec![50_000.0; 3], vec![30_000.0; 3]);
    w.total_executions = 80_000_000.0;
    let q = query(&mut s, "3432126458605143571");
    (q.executions, q.total_time_ms, q.rows) = (4_000_000.0, 4_000_000.0 * q.mean_time_ms, Some(4_000_000.0));
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    let report = analyze(s);
    let f = find(&report.findings, "chatty-query:3432126458605143571").expect("chatty over the scans");
    let metric = |label: &str| f.metrics.iter().find(|m| m.label == label).map(|m| m.value.round());
    assert_eq!(metric("Runs per day over Culprit's scans"), Some(30_000.0), "{:?}", f.metrics);
    assert_eq!(metric(&format!("Runs per day {totals}")), Some(100_000.0), "{:?}", f.metrics);
    let share = |label: &str| f.metrics.iter().find(|m| m.label == label).map(|m| (m.value * 100.0).round());
    assert_eq!(share("Share of executions over Culprit's scans"), Some(30.0), "{:?}", f.metrics);
    assert_eq!(share(&format!("Share of executions {totals}")), Some(5.0), "{:?}", f.metrics);
    assert!(metric("Average duration over Culprit's scans").is_some(), "{:?}", f.metrics);
    assert!(
        f.summary.contains("Its runs are 30% of all statement runs over 36 hours of Culprit's scans, and each run")
            && f.summary.contains(&format!(" (counted {totals})")),
        "{}",
        f.summary
    );

    // An unstable read: 1% of its 10K runs take 5 s, at the same average over the scans as over the totals.
    let (mean, sd, max, n) = spread_of(&[(2.0, 9_900.0), (5000.0, 100.0)]);
    let mut s = with_scans("3432126458605143571", 100.0, 100.0 * mean);
    let q = query(&mut s, "3432126458605143571");
    (q.executions, q.mean_time_ms, q.total_time_ms) = (n, mean, mean * n);
    (q.stddev_time_ms, q.max_time_ms) = (Some(sd), Some(max));
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    let report = analyze(s);
    let f = find(&report.findings, "unstable-query:3432126458605143571").expect("unstable over the scans");
    assert!(f.summary.contains(&format!("more than the average, together {totals}.")), "{}", f.summary);
    assert!(f.metrics.iter().any(|m| m.label == "Average over Culprit's scans"), "{:?}", f.metrics);
    let extra = format!("Slow runs' extra time {totals} (at least)");
    assert!(f.metrics.iter().any(|m| m.label == extra), "{:?}", f.metrics);
}

// The stopped statement's measured share is retained without inventing a wait cause from its small read count.
#[test]
fn a_stopped_heavy_query_keeps_its_measured_share_without_an_inferred_wait_cause() {
    let mut s = snapshot("postgres-demo.json");
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-10-05T11:54:21Z".into()), 144.0);
    let q = query(&mut s, "4911431806993420100");
    (q.executions, q.total_time_ms, q.logical_reads) = (50.0, 50.0 * q.mean_time_ms, Some(50.0 * 15.0));
    (q.first_seen, q.last_seen) = (Some("2026-10-06T00:00:00Z".into()), Some("2026-10-08T00:00:00Z".into()));
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    let f = find(&analyze(s).findings, "heavy-query:4911431806993420100").cloned().expect("heavy");
    assert!(f.inactive, "{}", f.title);
    assert!(
        f.title.starts_with("An update of products took ") && f.title.ends_with(" of database time until it stopped"),
        "{}",
        f.title
    );
}

/// The heavy-query check alone, before the refine stage takes away a heavy query another finding explains.
fn heavy_queries(s: &Snapshot) -> Vec<culprit_core::Finding> {
    let check = culprit_checks::all().into_iter().find(|c| c.info().id == "heavy-query").expect("heavy-query");
    check.run(s)
}

// The aptabase-us reading: the six heavy places went by the totals since 2023, so an insert that stopped running before
// the first scan (30% of those totals) took one, and a read at 10.7% over Culprit's scans (5.2% of the totals) lost
// its finding. Over a day of scans the six are those that run, ranked by the scans; history follows them.
#[test]
fn a_stopped_statement_never_takes_a_heavy_place_of_one_that_runs() {
    let mut s = with_scans("9065391607317686058", 10.0, 2_000.0);
    let w = s.workload.as_mut().unwrap();
    let t = w.timeline.as_mut().unwrap();
    t.time_ms = vec![20_000.0; 3];
    // Seven statements that run, at 10%, 9.5% … 7% of the scans' time.
    let running = [
        "9065391607317686058",
        "5308963031944094559",
        "-3722457391793582113",
        "2004611025554862962",
        "2885972271926804069",
        "3432126458605143571",
        "8465053948014496855",
    ];
    for (i, id) in running.iter().enumerate() {
        let q = query(&mut s, id);
        q.executions = q.executions.max(30.0);
        q.trend = Some(culprit_core::QueryTrend {
            time_ms: vec![Some(2_000.0 - 100.0 * i as f64); 3],
            executions: vec![Some(10.0); 3],
        });
    }
    // An insert that stopped two and a half days ago: most of the totals since the reset, none of the scans.
    let stopped = query(&mut s, "4911431806993420100");
    (stopped.executions, stopped.total_time_ms, stopped.mean_time_ms) = (100_000.0, 10_000_000.0, 100.0);
    (stopped.first_seen, stopped.last_seen) = (None, Some("2026-10-09T00:00:00Z".into()));
    stopped.trend = Some(culprit_core::QueryTrend { time_ms: vec![Some(0.0); 3], executions: vec![Some(0.0); 3] });
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;

    let heavy = heavy_queries(&s);
    let ids: Vec<&str> = heavy.iter().map(|f| f.id.trim_start_matches("heavy-query:")).collect();
    // The six that run, by their share over the scans, then the one that stopped: the seventh has no place.
    assert_eq!(
        ids,
        [&running[..6], &["4911431806993420100"]].concat(),
        "{:?}",
        heavy.iter().map(|f| &f.title).collect::<Vec<_>>()
    );
    assert!(heavy[0].title.ends_with("takes 10% of database time"), "{}", heavy[0].title);
    // In the report the stopped one is history, ranked after every statement that runs.
    let report = analyze(s);
    let f = find(&report.findings, "heavy-query:4911431806993420100").expect("history");
    // What it took until it stopped: its share of the totals, the scans having measured none of it.
    assert!(f.inactive && f.title.contains("took 99% of database time"), "{}", f.title);
    assert!(find(&report.findings, "heavy-query:8465053948014496855").is_none());
}

#[test]
fn an_aggregate_is_not_titled_by_rows_read_per_row_returned() {
    // `SELECT COUNT(*) FROM orders WHERE status = ? AND created_at >= ?` returns one row whatever it counts.
    let report = analyze(snapshot("mysql-demo.json"));
    let f = find(&report.findings, "heavy-query:c7b5ac9bb4bcecfe").unwrap();
    assert!(!f.title.contains("for every row it returns"), "{}", f.title);
}

// ── the costliest step of a cheap plan ───────────────────────────────────────────────────────────

/// Query 7 as a lookup on a unique index, run thousands of times at a millisecond and returning its row each time (not
/// so often that it's a chatty query): a seek and a key lookup of `looked_up` rows each in a table of 247K rows (SQL
/// Server's TableCardinality), at `cost` in all.
fn point_lookup(looked_up: f64, cost: f64) -> Snapshot {
    let mut s = snapshot("mssql-demo.json");
    let q = query(&mut s, "7");
    (q.executions, q.mean_time_ms, q.total_time_ms, q.rows) = (3_000.0, 1.0, 3_000.0, Some(3_000.0));
    q.logical_reads = Some(6.2 * 3_000.0);
    let plan = q.plan.as_mut().expect("plan");
    // The demo's query compares a converted column; this one is the plain seek it stands in for.
    plan.warnings.clear();
    plan.props.remove("PlanAffectingConvert");
    plan.total_cost = cost;
    plan.root.subtree_cost = cost;
    plan.root.self_cost = 0.0;
    let (seek, lookup) = plan.root.children.split_at_mut(1);
    let (seek, lookup) = (&mut seek[0], &mut lookup[0]);
    (seek.op, seek.kind, seek.predicate) = ("Index Seek".into(), NodeKind::Seek, None);
    (seek.est_rows, seek.rows_read, seek.table_rows) = (looked_up, Some(looked_up), Some(247_435.0));
    (seek.self_cost, seek.subtree_cost) = (cost * 0.4, cost * 0.4);
    (lookup.op, lookup.kind, lookup.object) = ("Key Lookup".into(), NodeKind::Lookup, Some("shop.customers".into()));
    (lookup.est_rows, lookup.est_executions, lookup.rows_read) = (1.0, looked_up, None);
    lookup.table_rows = Some(247_435.0);
    (lookup.self_cost, lookup.subtree_cost) = (cost * 0.6, cost * 0.6);
    for n in [seek, lookup] {
        n.warnings.clear();
    }
    s
}

#[test]
fn a_point_lookups_costliest_step_is_not_where_its_time_goes() {
    // Half of a plan costing 0.0066 is a lookup of one row: not "a key lookup that reads 247K rows", and no covering
    // index to add. The time is in how often it runs.
    let report = analyze(point_lookup(1.0, 0.0066));
    let f = find(&report.findings, "heavy-query:7").expect("still the heaviest query");
    assert!(!f.summary.contains("247"), "{}", f.summary);
    assert!(!f.summary.contains("key lookup"), "{}", f.summary);
    assert!(f.summary.contains("Its plan is cheap"), "{}", f.summary);
    let fix = &f.fix.as_ref().unwrap().summary;
    assert!(!fix.contains("covering index"), "{fix}");
    assert!(fix.contains("how often it runs"), "{fix}");
    assert_eq!(f.queries[0].plan_node, None, "no step to point at");
    let brief = culprit_checks::agent_brief(&report, f);
    assert!(!brief.contains("most expensive step"), "{brief}");
}

#[test]
fn a_plan_costed_cheap_whose_runs_are_slow_is_not_called_cheap() {
    // Stale statistics: costed as a point lookup, measured at over a second a run. The estimate is what's wrong.
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    (q.executions, q.mean_time_ms, q.total_time_ms) = (1_000.0, 1_200.0, 1_200_000.0);
    let report = analyze(s);
    let f = find(&report.findings, "heavy-query:7").expect("heavy");
    assert!(!f.summary.contains("Its plan is cheap"), "{}", f.summary);
    assert!(!f.fix.as_ref().unwrap().summary.contains("already cheap"));
}

#[test]
fn many_key_lookups_in_a_plan_that_does_real_work_are_still_covered() {
    // 20K lookups in a plan costing 40: covering them is the fix, said once, by the plan's key-lookups finding that
    // explains the query's time. The rows it reads are the plan's, never the table's size.
    let report = analyze(point_lookup(20_000.0, 40.0));
    let f = report.findings.iter().find(|f| f.queries.iter().any(|q| q.id == "7")).expect("query 7");
    assert_eq!(f.id, "plan-insights:key-lookups:shop.customers");
    assert!(f.fix.as_ref().unwrap().summary.contains("as included columns"), "{:?}", f.fix);
    assert!(!f.summary.contains("247"), "{}", f.summary);
}

#[test]
fn a_plan_is_called_cheap_only_where_its_runs_measure_cheap() {
    // Each costed as a point lookup and run at a millisecond: what the runs read and return decides, and the summary
    // and the fix say it on the same condition.
    let cheap = |s: Snapshot| {
        let f = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy");
        let fix = f.fix.as_ref().unwrap().summary.clone();
        assert_eq!(f.summary.contains("Its plan is cheap"), fix.contains("already cheap"), "{} | {fix}", f.summary);
        f.summary.contains("Its plan is cheap")
    };
    assert!(cheap(point_lookup(1.0, 0.0066)));
    // 750 pages a run where the plan expected to read one row (stale statistics).
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    (q.logical_reads, q.rows) = (Some(750.0 * 3_000.0), Some(0.0));
    assert!(!cheap(s));
    // 201 rows a run where the plan expected one (a generic plan's guess).
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    (q.logical_reads, q.rows) = (Some(67.0 * 3_000.0), Some(201.0 * 3_000.0));
    assert!(!cheap(s));
    // 6 ms a run is under the 10 ms the summary asks of a cheap run: the fix says so too.
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    (q.mean_time_ms, q.total_time_ms) = (6.0, 18_000.0);
    assert!(cheap(s));
}

#[test]
fn a_read_that_returns_nothing_is_titled_by_its_reads_per_run() {
    // Per row its reads would be counted against a row it never returns; per run they're 750 pages for nothing.
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    (q.logical_reads, q.rows) = (Some(750.0 * 3_000.0), Some(0.0));
    let f = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy");
    assert!(f.title.ends_with("reads 750 pages a run to return nothing"), "{}", f.title);
    // A lookup that finds nothing reads a few pages: no title about its reads.
    let mut s = point_lookup(1.0, 0.0066);
    query(&mut s, "7").rows = Some(0.0);
    let f = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy");
    assert!(!f.title.contains("pages"), "{}", f.title);
}

#[test]
fn a_cheap_write_is_told_to_write_more_per_statement_not_to_cache() {
    let mut s = point_lookup(1.0, 0.0066);
    query(&mut s, "7").text = "INSERT INTO [shop].[customers] ([name], [email]) VALUES (@P1, @P2)".into();
    let f = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy");
    // A write's plan costs next to nothing whatever its runs cost: not "cheap", the write itself.
    assert!(!f.summary.contains("Its plan is cheap"), "{}", f.summary);
    assert!(f.summary.contains("what a run costs is the write itself") && f.summary.contains("Each run inserts 1 row"));
    let fix = f.fix.unwrap().summary;
    // SQL Server's words for many rows at once; its only unique key is the id it doesn't set, so one transaction
    // makes no other insert wait; and no index to drop where none goes unread.
    assert!(
        fix.contains(
            "Insert several rows per statement where the application has them together (a multi-row VALUES, a table-valued parameter, or a bulk insert), in one transaction."
        ),
        "{fix}"
    );
    assert!(!fix.contains("cache") && !fix.contains("skip") && !fix.contains("index"), "{fix}");
}

/// The PostgreSQL demo's lookup of customers turned into `text`, an insert of the values it is given into products
/// (2,000 runs at 5 ms, 15 pages each, `rows` rows changed in all, 1.7 KB of WAL a run), counted for 40 days.
fn heavy_insert(text: &str, rows: f64) -> Snapshot {
    let mut s = snapshot("postgres-demo.json");
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(24.0 * 40.0);
    let q = query(&mut s, "3432126458605143571");
    q.text = text.into();
    q.plan = Some(insert_plan_into("public.products"));
    (q.executions, q.mean_time_ms, q.total_time_ms) = (2_000.0, 5.0, 10_000.0);
    (q.rows, q.logical_reads) = (Some(rows), Some(15.0 * 2_000.0));
    q.costs = Some(culprit_core::QueryCosts {
        log_bytes: Some(2_000.0 * 1_700.0),
        log_share: Some(0.99),
        log_full_pages: Some(1_350.0),
        ..Default::default()
    });
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    s
}

// The aptabase readings: "skip the statement where the application already knows the row exists", verified by "its
// time and its reads per run should drop". Skipping cuts runs, and the runs left are the ones that write, which read
// more each: a developer would read that as the fix failing. A fix that cuts runs expects fewer runs and less time.
#[test]
fn a_fix_that_cuts_runs_expects_fewer_runs_never_fewer_reads() {
    // pg_stat_statements: the difference between two scans, in What changed, which carries runs and time, said the
    // CLI's way too.
    let report =
        analyze(heavy_insert("INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT DO NOTHING", 500.0));
    let f = find(&report.findings, "heavy-query:3432126458605143571").expect("a heavy insert");
    assert!(f.fix.as_ref().unwrap().summary.contains("skip the statement"), "{:?}", f.fix);
    let verify = f.verify.join(" ");
    assert!(
        verify.contains(
            "Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise."
        ),
        "{verify}"
    );
    assert!(!verify.contains("reads"), "{verify}");
    // Query Store: the next scan's window shows it.
    let report = analyze(point_lookup(1.0, 0.0066));
    let f = find(&report.findings, "heavy-query:7").expect("the heaviest query");
    assert_eq!(
        f.verify,
        [
            "After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."
        ]
    );
}

// A statement that stopped has nothing to verify: its own step ("there its time and its reads per run should drop …
// and this one then shows as stopped running") promised a change no scan shows, for a statement already stopped.
#[test]
fn a_finding_whose_statements_stopped_has_nothing_to_check() {
    let mut s = heavy_insert("INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT DO NOTHING", 500.0);
    // Three days of statistics, and none of its runs in the last 28 hours.
    s.workload.as_mut().unwrap().window_hours = 72.0;
    let q = query(&mut s, "3432126458605143571");
    (q.first_seen, q.last_seen, q.trend) = (None, Some("2026-10-10T08:00:00Z".into()), None);
    let report = analyze(s);
    let f = find(&report.findings, "heavy-query:3432126458605143571").expect("a heavy insert");
    assert!(f.inactive, "{:?}", f.caveats);
    assert_eq!(
        f.verify,
        ["Nothing to check unless it runs again: a later scan that sees it run lists this finding as current again."]
    );
    // Said once: the brief's codebase section doesn't say it again, nor that the totals keep the earlier runs.
    let brief = culprit_checks::agent_brief(&report, f);
    assert_eq!(brief.matches("Nothing to check").count(), 1, "{brief}");
    assert!(!brief.contains("keeps running totals") && !brief.contains("the way *How to verify* says"), "{brief}");
}

// The aptabase readings: `INSERT … ON CONFLICT DO NOTHING` took a quarter of database time at 0.56 ms a run, with 3 of
// 4 runs inserting nothing and 99% of the WAL. "Its plan is cheap, so the share comes from how often it runs", and a
// fix to batch "inside one transaction" (unstable-query on the same insert says keep those short), with a table-valued
// parameter (SQL Server's) and indexes to drop where none goes unread.
#[test]
fn a_write_whose_runs_mostly_change_nothing_is_told_to_skip_them() {
    let text = "INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT DO NOTHING";
    let report = analyze(heavy_insert(text, 500.0));
    let f = find(&report.findings, "heavy-query:3432126458605143571").expect("a heavy insert");
    assert!(!f.fix.as_ref().unwrap().summary.contains("every time it runs"));
    let rows = f.metrics.iter().find(|m| m.label == "Rows changed per run").unwrap();
    assert_eq!(culprit_checks::metric_text(rows), "0.25");
    assert!(!f.summary.contains("cheap") && !f.summary.contains("how often it runs"), "{}", f.summary);
    assert!(
        f.summary.contains(
            "Each run inserts 0.25 rows on average (75% of its runs insert nothing) and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote: 6.6 KB and 2.7 full-page images"
        ),
        "{}",
        f.summary
    );
    let fix = &f.fix.as_ref().unwrap().summary;
    assert!(
        fix.contains(
            "75% of its runs insert nothing: skip the statement where the application already knows the row exists"
        ),
        "{fix}"
    );
    for wrong in ["table-valued", "transaction", "Insert several rows"] {
        assert!(!fix.contains(wrong), "{wrong}: {fix}");
    }

    // A row a run: more rows per statement, in PostgreSQL's words. products_sku_key is unique on the sku it sets, so
    // another transaction inserting the same sku waits for this one: not "in one transaction". products_sku_idx has
    // had no reads in 40 days: the one index worth dropping.
    let f = analyze(heavy_insert("INSERT INTO products (sku, name) VALUES ($1, $2)", 2_000.0))
        .findings
        .into_iter()
        .find(|f| f.id == "heavy-query:3432126458605143571")
        .expect("a heavy insert");
    let fix = f.fix.unwrap().summary;
    assert!(
        fix.contains(
            "Insert several rows per statement where the application has them together (a multi-row VALUES, arrays unnested into rows, or COPY)."
        ),
        "{fix}"
    );
    assert!(!fix.contains("transaction") && !fix.contains("table-valued"), "{fix}");
    assert!(fix.contains("products_sku_idx has had no reads in the 40 days"), "{fix}");
    assert!(f.summary.contains("Each run inserts 1 row on average and writes 1.7 KB of WAL"), "{}", f.summary);
}

// Logging advice for a statement that explicitly requests a blocking advisory lock depends on how often it runs.
#[test]
fn a_lock_wait_too_rare_to_catch_live_is_logged_next_time() {
    let fix = |runs: f64| {
        let mut s = snapshot("postgres-demo.json");
        let q = query(&mut s, "4911431806993420100");
        q.text = "SELECT pg_advisory_xact_lock($1)".into();
        q.plan = None;
        (q.executions, q.total_time_ms, q.mean_time_ms) = (runs, runs * 19_000.0, 19_000.0);
        let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
        s.workload.as_mut().unwrap().total_time_ms = total;
        find(&analyze(s).findings, "heavy-query:4911431806993420100")
            .cloned()
            .expect("a statement requesting an advisory lock")
            .fix
            .unwrap()
    };
    let rare = fix(3.0);
    assert!(
        rare.summary.starts_with(
            "It ran 3 times in 5 hours, too seldom to count on catching it while it waits. Have the next ones logged instead. Set log_lock_waits to on"
        ),
        "{}",
        rare.summary
    );
    assert!(rare.summary.contains("logs each lock wait longer than deadlock_timeout"), "{}", rare.summary);
    assert!(!rare.summary.contains("Live shows"), "{}", rare.summary);
    assert!(
        rare.sql.as_deref().is_some_and(|sql| sql.contains("ALTER SYSTEM SET log_lock_waits = 'on'")),
        "{:?}",
        rare.sql
    );
    // Twenty runs an hour: there are waits to catch while they happen.
    let often = fix(100.0);
    assert!(
        often.summary.starts_with("Find what holds the locks while it waits (Live shows the blocking chain)."),
        "{}",
        often.summary
    );
    assert_eq!(often.sql, None);
}

// US-4: the aptabase-us reading said "Each run inserts 1 row on average" and "Insert several rows per statement" for an
// `INSERT … ON CONFLICT … DO UPDATE SET salt = app_salts.salt RETURNING salt`: a get-or-create whose rows count the
// rows it rewrote, each a new row version and its WAL. Its levers are not sending it when the row is known, and
// DO NOTHING with a read on a miss; an upsert with new values updates only rows that differ.
#[test]
fn an_upsert_is_said_to_insert_or_update_and_a_get_or_create_is_fixed_as_one() {
    let get_or_create = "INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT (sku) DO UPDATE SET name = products.name RETURNING name";
    let f = find(&analyze(heavy_insert(get_or_create, 2_000.0)).findings, "heavy-query:3432126458605143571")
        .cloned()
        .expect("a heavy upsert");
    assert!(
        f.summary.contains("Each run inserts or updates 1 row on average (a run that finds the row there rewrites it"),
        "{}",
        f.summary
    );
    let fix = f.fix.unwrap().summary;
    assert!(fix.contains("It is a get-or-create: its DO UPDATE sets a column to the value it has"), "{fix}");
    assert!(
        fix.contains("evaluate ON CONFLICT DO NOTHING RETURNING followed by a separate SELECT when no row is returned"),
        "{fix}"
    );
    for safeguard in [
        "not an equivalent atomic replacement",
        "transaction isolation",
        "concurrent deletion",
        "serialization failures",
        "locking or retries",
        "UPDATE-trigger effects",
        "keep the atomic upsert",
    ] {
        assert!(fix.contains(safeguard), "{fix}");
    }
    assert!(!fix.contains("several rows per statement"), "{fix}");
    let upsert =
        "INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT (sku) DO UPDATE SET name = EXCLUDED.name";
    let f = find(&analyze(heavy_insert(upsert, 2_000.0)).findings, "heavy-query:3432126458605143571")
        .cloned()
        .expect("a heavy upsert");
    assert!(f.summary.contains("Each run inserts or updates 1 row on average and writes"), "{}", f.summary);
    let fix = f.fix.unwrap().summary;
    assert!(fix.contains("give its DO UPDATE a WHERE that updates only a row whose values differ"), "{fix}");
}

// The aptabase-us reading: "An insert into app_salts takes 30% of database time" for an insert silent since Culprit's
// first scan.
#[test]
fn a_statement_that_stopped_is_titled_in_the_past() {
    let mut s = heavy_insert("INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT DO NOTHING", 500.0);
    // Quiet for the last four hours of the window: too soon to say it stopped, so it is told as it runs.
    query(&mut s, "3432126458605143571").last_seen = Some("2026-10-11T08:00:00Z".into());
    let f = find(&analyze(s.clone()).findings, "heavy-query:3432126458605143571").cloned().expect("a heavy insert");
    assert!(!f.inactive);
    assert!(f.title.starts_with("An insert into products takes "), "{}", f.title);
    // Quiet since the morning before: a day of silence. Its title, summary and fix are in the past.
    let q = query(&mut s, "3432126458605143571");
    q.first_seen = Some("2026-10-10T06:00:00Z".into());
    q.last_seen = Some("2026-10-10T08:00:00Z".into());
    let report = analyze(s);
    let f = find(&report.findings, "heavy-query:3432126458605143571").expect("a heavy insert");
    assert!(f.inactive);
    assert!(
        f.title.starts_with("An insert into products took ") && f.title.ends_with(" until it stopped"),
        "{}",
        f.title
    );
    assert!(
        f.summary.contains(
            "Its plan had nothing to make faster: checking for rows still cost work when a run changed nothing"
        ),
        "{}",
        f.summary
    );
    assert!(f.summary.contains(" and wrote 1.7 KB of WAL"), "{}", f.summary);
}

#[test]
fn a_material_native_wait_is_a_lower_bound_without_an_elapsed_share_or_cache_cause() {
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    q.waits = vec![WaitStat {
        category: "IO".into(),
        wait_type: Some("Buffer IO".into()),
        wait_ms: q.total_time_ms * 0.85,
        tasks: None,
    }];
    let f = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy");
    assert!(
        f.summary.contains("Query Store recorded at least 2.5 s of waiting (Buffer IO) for this statement."),
        "{}",
        f.summary
    );
    for unsupported in ["85%", "wasn't in memory", "eviction", "insufficient memory"] {
        assert!(!f.summary.contains(unsupported), "{unsupported}: {}", f.summary);
    }
    // The materiality threshold determines which total is highlighted, not what fraction of elapsed time it owns.
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    q.waits = vec![WaitStat {
        category: "IO".into(),
        wait_type: Some("Buffer IO".into()),
        wait_ms: q.total_time_ms * 0.3,
        tasks: None,
    }];
    let f = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy");
    assert!(!f.summary.contains("Buffer IO"), "{}", f.summary);
}

// A locking clause and a few page reads do not prove that another transaction blocked these runs.
#[test]
fn a_locking_read_without_native_wait_evidence_keeps_its_cause_unknown() {
    let mut s = snapshot("postgres-demo.json");
    let q = query(&mut s, "4911431806993420100");
    q.text = "SELECT id FROM orders WHERE id = $1 FOR UPDATE".into();
    (q.executions, q.mean_time_ms, q.total_time_ms) = (3.0, 38_412.0, 115_236.0);
    (q.rows, q.logical_reads, q.plan) = (Some(3.0), Some(27.0), None);
    let total: f64 = s.workload.as_ref().unwrap().queries.iter().map(|q| q.total_time_ms).sum();
    s.workload.as_mut().unwrap().total_time_ms = total;
    let report = analyze(s);
    let f = find(&report.findings, "heavy-query:4911431806993420100").expect("the locking read");
    assert!(f.title.contains("2 min of database time"), "{}", f.title);
    assert!(!f.title.contains("waiting") && !f.title.contains("locks"), "{}", f.title);
    assert!(!f.summary.contains("most likely") && !f.summary.contains("another transaction held"), "{}", f.summary);
    let fix = &f.fix.as_ref().unwrap().summary;
    assert!(!fix.contains("Find what holds the locks") && !fix.contains("won't shorten the wait"), "{fix}");
    assert!(f.metrics.iter().any(|m| m.label == "Rows returned per run"), "{:?}", f.metrics);
}

// A staging SQL Server reading: "with a unique index on those columns, an insert that handles the duplicate-key error… needs
// no read first" for an idempotency check whose plan seeks that very unique index: it reads as one to create.
#[test]
fn a_check_for_a_row_names_the_unique_index_its_plan_seeks() {
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    q.rows = Some(1.0);
    q.waits = vec![WaitStat {
        category: "IO".into(),
        wait_type: Some("Buffer IO".into()),
        wait_ms: q.total_time_ms * 0.86,
        tasks: None,
    }];
    s.indexes.iter_mut().find(|i| i.name == "ix_customers_email").unwrap().is_unique = true;
    let fix = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy").fix.unwrap().summary;
    assert!(
        fix.contains("this plan seeks ix_customers_email on shop.customers, a unique index that exists already, so an insert that handles its duplicate-key error (or an upsert) needs no read first"),
        "{fix}"
    );
    assert!(!fix.contains("with a unique index on those columns"), "{fix}");
}

/// A residual filter in an estimated plan beside many recorded pages and no returned row. Large values, repeated
/// page accesses or another plan can account for those pages: they do not establish an intermediate row count.
fn lookups_for_nothing() -> Snapshot {
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    q.text = "SELECT [id], [name] FROM [shop].[customers] WHERE [email] = @P1 AND [segment] = @P2".into();
    (q.logical_reads, q.rows) = (Some(750.0 * 3_000.0), Some(0.0));
    let plan = q.plan.as_mut().expect("plan");
    plan.root.children[0].seek_predicate = Some("[email] = [@P1]".into());
    plan.root.children[1].predicate = Some("[culprit_demo].[shop].[customers].[segment]=[@P2]".into());
    s
}

#[test]
fn pages_read_by_a_residual_lookup_establish_no_row_count_or_statistics_fix() {
    for has_other_index in [false, true] {
        let mut s = lookups_for_nothing();
        if has_other_index {
            let mut segment = s.indexes.iter().find(|i| i.name == "ix_customers_email").cloned().unwrap();
            (segment.name, segment.columns) = ("ix_customers_segment".into(), vec!["segment".into()]);
            s.indexes.push(segment);
        }
        let report = analyze(s);
        let f = find(&report.findings, "heavy-query:7").expect("heavy");
        assert!(f.title.ends_with("reads 750 pages a run to return nothing"), "{}", f.title);
        assert!(f.summary.contains("The captured plan estimates 1 row from ix_customers_email"), "{}", f.summary);
        assert!(f.summary.contains("those totals do not show how many rows each plan step handled"), "{}", f.summary);
        assert!(!f.summary.contains("250 lookups") && !f.summary.contains("far fewer rows"), "{}", f.summary);
        assert_eq!(f.queries[0].plan_node, Some(4));
        let fix = f.fix.as_ref().unwrap();
        assert!(fix.sql.is_none(), "{fix:?}");
        assert!(fix.summary.contains("representative actual plan"), "{}", fix.summary);
        assert!(!fix.summary.contains("UPDATE STATISTICS") && !fix.summary.contains("FORCESEEK"), "{}", fix.summary);
        assert!(f.verify[0].contains("IO"), "{:?}", f.verify);
        let brief = culprit_checks::agent_brief(&report, f);
        assert!(!brief.contains("about 250 lookups") && !brief.contains("UPDATE STATISTICS"), "{brief}");
    }
}

#[test]
fn storage_wait_share_does_not_diagnose_eviction_or_insufficient_memory() {
    // An idempotency check: 1 row in 3K runs of a few pages each, with a material native I/O wait total.
    let mut s = point_lookup(1.0, 0.0066);
    let q = query(&mut s, "7");
    q.rows = Some(1.0);
    q.waits = vec![WaitStat {
        category: "IO".into(),
        wait_type: Some("Buffer IO".into()),
        wait_ms: q.total_time_ms * 0.86,
        tasks: None,
    }];
    let f = find(&analyze(s).findings, "heavy-query:7").cloned().expect("heavy");
    assert!(!f.summary.contains("the share comes from how often it runs"), "{}", f.summary);
    assert!(f.summary.contains("Its plan is cheap: each run reads about 6 pages"), "{}", f.summary);
    // Verification compares equivalent useful work rather than promising one particular cause or improvement.
    assert!(
        f.verify
            .iter()
            .any(|v| v.contains("I/O measurements and execution time") && v.contains("equivalent useful work")),
        "{:?}",
        f.verify
    );
    let fix = f.fix.unwrap().summary;
    assert!(
        fix.contains("physical reads and storage latency") && fix.contains("independent cache-pressure evidence"),
        "{fix}"
    );
    assert!(fix.contains("does not establish a cache miss, eviction or insufficient memory"), "{fix}");
    assert!(!fix.contains("cache the result") && !fix.contains("result cached"), "{fix}");
    assert!(fix.contains("a duplicate or idempotency check"), "{fix}");
    // One row in 3K runs reads as what it is, not as "0.0" rows a run.
    let rows = f.metrics.iter().find(|m| m.label.starts_with("Rows returned")).expect("rows");
    assert_eq!((rows.label.as_str(), rows.text.as_deref()), ("Rows returned", Some("1 row in 3.0K runs")));
}

#[test]
fn a_step_beside_one_the_plan_puts_no_cost_on_is_given_no_share() {
    // MySQL's filesort carries no cost: beside it, the scan reads as 100% of the plan's. Said plainly (MY-8), not as
    // "the step its plan costs highest… so no share is given" for a share the summary never mentioned.
    let report = analyze(snapshot("mysql-demo.json"));
    let f = find(&report.findings, "heavy-query:19bd4ff468839ea3").expect("the promo-code read");
    assert!(!f.summary.contains("of its estimated cost") && !f.summary.contains("share"), "{}", f.summary);
    assert!(
        f.summary.ends_with(
            "Its only step with a cost is a table scan of shop.orders that examines 500K rows a run (MySQL puts no cost on the sort)."
        ),
        "{}",
        f.summary
    );
}

#[test]
fn no_fix_tells_its_reader_to_copy_the_brief_it_is_in() {
    for name in ["mssql-demo.json", "postgres-demo.json", "mysql-demo.json"] {
        for f in analyze(snapshot(name)).findings {
            let fix = f.fix.map(|x| x.summary).unwrap_or_default();
            assert!(!fix.contains("copy the brief"), "{}: {fix}", f.id);
        }
    }
}

#[test]
fn mysqls_rows_examined_are_rows_and_measured_where_the_plan_reads_one_table() {
    // `SELECT COUNT(*) FROM orders WHERE status = ? AND created_at >= ?`: the plan estimates 29K rows read, the runs
    // examined 11K each.
    let report = analyze(snapshot("mysql-demo.json"));
    let f = find(&report.findings, "heavy-query:c7b5ac9bb4bcecfe").unwrap();
    assert!(f.summary.contains("an index range scan of shop.orders that examines 11K rows a run"), "{}", f.summary);
    assert!(!f.summary.contains("29K"), "{}", f.summary);
    let examined = f.metrics.iter().find(|m| m.label == "Rows examined per run").expect("rows examined");
    assert_eq!(examined.unit, Unit::Rows);
}

// ── the slow query log ───────────────────────────────────────────────────────────────────────────

fn slow_log(mut s: Snapshot) -> Snapshot {
    let w = s.workload.as_mut().unwrap();
    w.source = "slow query log".into();
    w.kind = Some(culprit_core::WorkloadKind::SlowLog);
    w.windowed = true;
    set(&mut s, "long_query_time", "0.100000");
    s
}

#[test]
fn slow_log_shares_remain_of_logged_runs_even_with_a_current_zero_threshold() {
    let report = analyze(slow_log(snapshot("mysql-demo.json")));
    let f = find(&report.findings, "heavy-query:c7b5ac9bb4bcecfe").unwrap();
    assert!(f.title.contains("of logged-run time over"), "{}", f.title);
    assert!(f.impact.label.contains("of logged-run time over"), "{}", f.impact.label);
    // The current threshold cannot establish that historical records contain every database execution.
    let mut s = slow_log(snapshot("mysql-demo.json"));
    set(&mut s, "long_query_time", "0.000000");
    let f = find(&analyze(s).findings, "heavy-query:c7b5ac9bb4bcecfe").cloned().unwrap();
    assert!(f.title.contains("of logged-run time over"), "{}", f.title);
    assert!(f.impact.label.contains("of logged-run time over"), "{}", f.impact.label);
    assert!(!f.title.contains("of database time"));
}

fn retained_distribution(median: f64, p99: f64, samples: u64, bins: &[[f64; 3]]) -> culprit_core::RuntimeDistribution {
    use culprit_core::{
        DistributionPopulation, RuntimeDistribution, RuntimeDistributionOrigin, RuntimeDuration,
        RuntimeHistogramBucket, RuntimeQuantile,
    };
    RuntimeDistribution {
        source: "mysql.slow_log".into(),
        origin: RuntimeDistributionOrigin::CapturedRuns,
        population: DistributionPopulation::CapturedSamples { sample_count: samples.to_string() },
        p50: Some(RuntimeQuantile::sample(median)),
        p95: None,
        p99: Some(RuntimeQuantile::sample(p99)),
        histogram: bins
            .iter()
            .map(|[lower, upper, count]| RuntimeHistogramBucket {
                lower: RuntimeDuration::Milliseconds { value: *lower },
                upper: RuntimeDuration::Milliseconds { value: *upper },
                samples: (*count as u64).to_string(),
            })
            .collect(),
        issue: None,
    }
}

#[test]
fn a_slow_log_tail_says_it_is_of_logged_runs_and_needs_several() {
    let mut s = slow_log(snapshot("mysql-demo.json"));
    let q = query(&mut s, "c7b5ac9bb4bcecfe");
    q.executions = 80.0;
    q.spread = Some(Spread {
        distributions: vec![retained_distribution(
            150.0,
            4000.0,
            80,
            &[[100.0, 200.0, 70.0], [200.0, 400.0, 7.0], [2000.0, 5000.0, 3.0]],
        )],
        ..Default::default()
    });
    let report = analyze(s.clone());
    let f = find(&report.findings, "parameter-sensitive:c7b5ac9bb4bcecfe").expect("three slow logged runs");
    assert!(f.title.starts_with("Recorded run times for a read from orders: median 150 ms"), "{}", f.title);
    assert!(
        f.summary.contains(
            "long_query_time is currently 100 ms; historical thresholds, filters and coverage are not established"
        ),
        "{}",
        f.summary
    );
    assert!(f.summary.contains("3 of 80 recorded histogram samples"), "{}", f.summary);
    assert_eq!(
        f.metrics.iter().find(|m| m.is("check.parameter_sensitive.metric.histogram_samples")).unwrap().value,
        80.0
    );
    assert_eq!(
        f.metrics.iter().find(|m| m.is("check.parameter_sensitive.metric.slow_runs")).unwrap().value,
        3.0 / 80.0
    );
    assert!(f.fix.as_ref().unwrap().sql.is_none());
    // Two slow runs are not a tail.
    query(&mut s, "c7b5ac9bb4bcecfe").spread.as_mut().unwrap().distributions[0] =
        retained_distribution(150.0, 4000.0, 79, &[[100.0, 200.0, 70.0], [200.0, 400.0, 7.0], [2000.0, 5000.0, 2.0]]);
    assert!(find(&analyze(s).findings, "parameter-sensitive:c7b5ac9bb4bcecfe").is_none());
}

// Recorded plan means and runtime tails retain their measured populations and compilation context. Settings,
// engine versions, native lock totals and compile costs alone do not authorize a mutation or identify the cause.
#[test]
fn recorded_runtime_variation_words_preserve_populations_and_keep_causes_unresolved() {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    assert_eq!(
        crate::info("parameter-sensitive"),
        "Recorded query runtime differences\n\
        Different plan averages or a long runtime tail identify statements worth investigating. They do not establish input-dependent plan reuse; the cause requires further evidence."
    );
    let said = |s: Snapshot, id: &str| {
        let findings = crate::run("parameter-sensitive", &s);
        let finding =
            findings.iter().find(|f| f.id == format!("parameter-sensitive:{id}")).expect("recorded variation");
        assert_eq!(finding.actionability, culprit_core::Actionability::Information);
        assert_eq!(finding.confidence, culprit_core::Confidence::Medium);
        assert!(finding.facts.runtime_variation.is_some());
        assert_eq!(finding.queries[0].id, id);
        let fix = finding.fix.as_ref().expect("bounded investigation");
        assert!(fix.sql.is_none(), "{id}: a population difference is not mutation authority");
        assert!(fix.cost.is_none(), "{id}: compile counters do not price a hypothetical remedy");
        crate::words(finding)
    };
    let side_by_side = || {
        with_plans(vec![
            plan("8", 900.0, 9.0, "2026-10-11T07:44:00Z", "2026-10-11T12:12:00Z"),
            plan("47", 100.0, 300.0, "2026-10-11T07:50:00Z", "2026-10-11T12:13:00Z"),
        ])
    };
    let several_fast = with_plans(vec![
        plan("8", 500.0, 2.0, "2026-10-11T07:44:00Z", "2026-10-11T12:12:00Z"),
        plan("9", 450.0, 3.0, "2026-10-11T07:45:00Z", "2026-10-11T12:11:00Z"),
        plan("47", 100.0, 300.0, "2026-10-11T07:50:00Z", "2026-10-11T12:13:00Z"),
    ]);
    let mut costly = side_by_side();
    let q = query(&mut costly, "8");
    q.costs = Some(culprit_core::QueryCosts { plan_ms: Some(4_000.0), plans: Some(1.0), ..Default::default() });
    for plan in &mut q.plans {
        plan.compiled = Some(format!("@P1 = ({})", plan.id));
    }
    costly.tuning = Some(TuningAdvice {
        automatic_tuning: vec![culprit_core::AutomaticTuningOption {
            name: "FORCE_LAST_GOOD_PLAN".into(),
            desired: "ON".into(),
            actual: "ON".into(),
            reason: None,
        }],
        ..Default::default()
    });
    let mut sql2019 = side_by_side();
    sql2019.server.version = "15.0.4355.3".into();
    query(&mut sql2019, "8").plan.as_mut().unwrap().facts_mut().compile_ms = None;
    let mut psp_off = side_by_side();
    set(&mut psp_off, "PARAMETER_SENSITIVE_PLAN_OPTIMIZATION", "OFF");
    let mut stopped = side_by_side();
    let q = query(&mut stopped, "8");
    let end = epoch_seconds("2026-10-11T12:13:47Z").unwrap();
    (q.first_seen, q.last_seen) =
        (Some(iso_from_epoch(end - 72.0 * 3600.0)), Some(iso_from_epoch(end - 48.0 * 3600.0)));
    let plans = [
        said(side_by_side(), "8"),
        said(several_fast, "8"),
        said(costly, "8"),
        said(sql2019, "8"),
        said(psp_off, "8"),
        said(stopped, "8"),
    ];
    const PLANS: &str = r###"Recorded plan averages for a read from orders: 9 ms versus 300 ms · Low · Medium
Plan 8 averaged 9 ms across 900 recorded executions; plan 47 averaged 300 ms across 100 recorded executions. Their execution conditions are not established as equivalent.
impact: Recorded statement workload
metric: Recorded statement time
metric: Faster recorded plan average
metric: Slower recorded plan average
metric: Recorded slower-plan time
metric: Recorded slower-plan executions
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded plan averages for a read from orders: 2 ms versus 300 ms · Low · Medium
Plan 8 averaged 2 ms across 500 recorded executions; plan 47 averaged 300 ms across 100 recorded executions. Their execution conditions are not established as equivalent.
impact: Recorded statement workload
metric: Recorded statement time
metric: Faster recorded plan average
metric: Slower recorded plan average
metric: Recorded slower-plan time
metric: Recorded slower-plan executions
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded plan averages for a read from orders: 9 ms versus 300 ms · Low · Medium
Plan 8 averaged 9 ms across 900 recorded executions; plan 47 averaged 300 ms across 100 recorded executions. Their execution conditions are not established as equivalent.
impact: Recorded statement workload
metric: Recorded statement time
metric: Faster recorded plan average
metric: Slower recorded plan average
metric: Recorded slower-plan time
metric: Recorded slower-plan executions
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Plan 8 has recorded compilation context `@P1 = (8)`; it does not establish the inputs of its recorded executions.
caveat: Plan 47 has recorded compilation context `@P1 = (47)`; it does not establish the inputs of its recorded executions.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded plan averages for a read from orders: 9 ms versus 300 ms · Low · Medium
Plan 8 averaged 9 ms across 900 recorded executions; plan 47 averaged 300 ms across 100 recorded executions. Their execution conditions are not established as equivalent.
impact: Recorded statement workload
metric: Recorded statement time
metric: Faster recorded plan average
metric: Slower recorded plan average
metric: Recorded slower-plan time
metric: Recorded slower-plan executions
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded plan averages for a read from orders: 9 ms versus 300 ms · Low · Medium
Plan 8 averaged 9 ms across 900 recorded executions; plan 47 averaged 300 ms across 100 recorded executions. Their execution conditions are not established as equivalent.
impact: Recorded statement workload
metric: Recorded statement time
metric: Faster recorded plan average
metric: Slower recorded plan average
metric: Recorded slower-plan time
metric: Recorded slower-plan executions
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded plan averages for a read from orders: 9 ms versus 300 ms · Low · Medium
Plan 8 averaged 9 ms across 900 recorded executions; plan 47 averaged 300 ms across 100 recorded executions. Their execution conditions are not established as equivalent.
impact: Recorded statement workload
metric: Recorded statement time
metric: Faster recorded plan average
metric: Slower recorded plan average
metric: Recorded slower-plan time
metric: Recorded slower-plan executions
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse."###;
    crate::pins::pin_text("parameter-variation-plans", &plans.join("\n\n"), PLANS);

    const ID: &str = "c7b5ac9bb4bcecfe";
    let tail = |mut s: Snapshot, runs: f64, histogram: bool| {
        use culprit_core::{
            DistributionPopulation, RuntimeDistribution, RuntimeDistributionOrigin, RuntimeDuration,
            RuntimeHistogramBucket, RuntimeQuantile,
        };
        let captured = s.workload.as_ref().unwrap().kind == Some(culprit_core::WorkloadKind::SlowLog);
        let bins = if histogram {
            vec![[0.0, 2.0, runs * 0.6], [2.0, 4.0, runs * 0.37], [500.0, 1000.0, runs * 0.03]]
        } else {
            Vec::new()
        };
        let distribution = if captured {
            retained_distribution(2.0, 900.0, runs as u64, &bins)
        } else if histogram {
            RuntimeDistribution::native_histogram(
                "performance_schema.events_statements_histogram_by_digest",
                bins.iter()
                    .map(|[lower, upper, count]| RuntimeHistogramBucket {
                        lower: RuntimeDuration::Milliseconds { value: *lower },
                        upper: RuntimeDuration::Milliseconds { value: *upper },
                        samples: (*count as u64).to_string(),
                    })
                    .collect(),
            )
        } else {
            RuntimeDistribution {
                source: "performance_schema.events_statements_summary_by_digest".into(),
                origin: RuntimeDistributionOrigin::StatementSummary,
                population: DistributionPopulation::NativeCounters { sample_count: None },
                p50: None,
                p95: None,
                p99: Some(RuntimeQuantile::HistogramUpperBound {
                    lower: None,
                    upper: RuntimeDuration::Milliseconds { value: 900.0 },
                }),
                histogram: Vec::new(),
                issue: None,
            }
        };
        let q = query(&mut s, ID);
        q.executions = runs;
        q.spread = Some(Spread { distributions: vec![distribution], ..Default::default() });
        s
    };
    let mysql = || {
        let mut s = snapshot("mysql-demo.json");
        // Uptime does not establish why a recorded run was slow.
        s.pressure.get_or_insert_default().uptime_hours = Some(72.0);
        s
    };
    let mut current_zero_threshold = tail(slow_log(mysql()), 1000.0, true);
    set(&mut current_zero_threshold, "long_query_time", "0.000000");
    let mut no_threshold = tail(slow_log(mysql()), 1000.0, true);
    no_threshold.settings.retain(|s| s.name != "long_query_time");
    let mut locked = tail(mysql(), 1000.0, true);
    let q = query(&mut locked, ID);
    q.spread.as_mut().unwrap().lock_ms = Some(q.total_time_ms * 0.4);
    let mut mariadb = tail(mysql(), 1000.0, true);
    (mariadb.server.product, mariadb.server.version) = ("MariaDB 10.11.8".into(), "10.11.8-MariaDB".into());
    let mut stopped = tail(mysql(), 1000.0, false);
    let end = epoch_seconds(stopped.workload.as_ref().unwrap().window_end.as_deref().unwrap()).unwrap();
    let q = query(&mut stopped, ID);
    (q.first_seen, q.last_seen, q.max_time_ms) =
        (Some(iso_from_epoch(end - 72.0 * 3600.0)), Some(iso_from_epoch(end - 48.0 * 3600.0)), None);
    let native_quantile = tail(mysql(), 400.0, false);
    for unknown_population in [&native_quantile, &stopped] {
        assert!(
            crate::run("parameter-sensitive", unknown_population)
                .iter()
                .all(|finding| finding.id != format!("parameter-sensitive:{ID}")),
            "a native upper estimate without its own count/median/lower bound cannot establish a tail"
        );
    }
    let tails = [
        said(tail(mysql(), 1000.0, true), ID),
        said(current_zero_threshold, ID),
        said(no_threshold, ID),
        said(locked, ID),
        said(mariadb, ID),
    ];
    const TAILS: &str = r###"Recorded histogram bounds for a read from orders: p50 up to 2 ms, p99 500 ms–1.0 s · Low · Medium
Native distribution from performance_schema.events_statements_histogram_by_digest: the recorded p50 bucket ends at 2 ms; the p99 bucket spans 500 ms to 1.0 s. These are bucket bounds, not exact execution durations. 30 of 1.0K recorded histogram samples have bucket lower bounds at or above 20 ms.
impact: Recorded statement workload
metric: Recorded statement time
metric: Recorded median bucket upper bound
metric: Recorded p99 bucket upper bound
metric: Recorded histogram samples
metric: Samples in slow histogram buckets
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: This native distribution was observed independently. Its reset and sample population are not established as the statement-summary population; statement totals provide context rather than the cost of this tail.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded run times for a read from orders: median 2 ms, 99th percentile 900 ms · Low · Medium
Among the retained logged runs of this statement (long_query_time is currently 0 ms; historical thresholds, filters and coverage are not established): the recorded median is 2 ms and the 99th percentile is 900 ms. 30 of 1.0K recorded histogram samples have bucket lower bounds at or above 20 ms.
impact: Recorded statement workload
metric: Recorded statement time
metric: Recorded median
metric: Recorded 99th percentile
metric: Recorded histogram samples
metric: Samples in slow histogram buckets
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded run times for a read from orders: median 2 ms, 99th percentile 900 ms · Low · Medium
Among the retained logged runs of this statement (logging thresholds, filters and retention may omit other executions): the recorded median is 2 ms and the 99th percentile is 900 ms. 30 of 1.0K recorded histogram samples have bucket lower bounds at or above 20 ms.
impact: Recorded statement workload
metric: Recorded statement time
metric: Recorded median
metric: Recorded 99th percentile
metric: Recorded histogram samples
metric: Samples in slow histogram buckets
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded histogram bounds for a read from orders: p50 up to 2 ms, p99 500 ms–1.0 s · Low · Medium
Native distribution from performance_schema.events_statements_histogram_by_digest: the recorded p50 bucket ends at 2 ms; the p99 bucket spans 500 ms to 1.0 s. These are bucket bounds, not exact execution durations. 30 of 1.0K recorded histogram samples have bucket lower bounds at or above 20 ms.
impact: Recorded statement workload
metric: Recorded statement time
metric: Recorded median bucket upper bound
metric: Recorded p99 bucket upper bound
metric: Recorded histogram samples
metric: Samples in slow histogram buckets
metric: Recorded lock timing
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: This native distribution was observed independently. Its reset and sample population are not established as the statement-summary population; statement totals provide context rather than the cost of this tail.
caveat: Recorded lock timing does not identify which calls waited or establish that locks explain this tail.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded histogram bounds for a read from orders: p50 up to 2 ms, p99 500 ms–1.0 s · Low · Medium
Native distribution from performance_schema.events_statements_histogram_by_digest: the recorded p50 bucket ends at 2 ms; the p99 bucket spans 500 ms to 1.0 s. These are bucket bounds, not exact execution durations. 30 of 1.0K recorded histogram samples have bucket lower bounds at or above 20 ms.
impact: Recorded statement workload
metric: Recorded statement time
metric: Recorded median bucket upper bound
metric: Recorded p99 bucket upper bound
metric: Recorded histogram samples
metric: Samples in slow histogram buckets
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: This native distribution was observed independently. Its reset and sample population are not established as the statement-summary population; statement totals provide context rather than the cost of this tail.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse."###;
    crate::pins::pin_text("parameter-variation-tails", &tails.join("\n\n"), TAILS);

    let mut cheap = side_by_side();
    query(&mut cheap, "8").costs =
        Some(culprit_core::QueryCosts { plan_ms: Some(1.0), plans: Some(1.0), ..Default::default() });
    let mut few = slow_log(mysql());
    let q = query(&mut few, ID);
    q.executions = 80.0;
    q.spread = Some(Spread {
        distributions: vec![retained_distribution(
            150.0,
            4000.0,
            80,
            &[[100.0, 200.0, 70.0], [200.0, 400.0, 7.0], [2000.0, 5000.0, 3.0]],
        )],
        ..Default::default()
    });
    let followup = [said(tail(slow_log(mysql()), 400.0, false), ID), said(cheap, "8"), said(few, ID)];
    const FOLLOWUP: &str = r###"Recorded run times for a read from orders: median 2 ms, 99th percentile 900 ms · Low · Medium
Among the retained logged runs of this statement (long_query_time is currently 100 ms; historical thresholds, filters and coverage are not established): the recorded median is 2 ms and the 99th percentile is 900 ms.
impact: Recorded statement workload
metric: Recorded statement time
metric: Recorded median
metric: Recorded 99th percentile
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded plan averages for a read from orders: 9 ms versus 300 ms · Low · Medium
Plan 8 averaged 9 ms across 900 recorded executions; plan 47 averaged 300 ms across 100 recorded executions. Their execution conditions are not established as equivalent.
impact: Recorded statement workload
metric: Recorded statement time
metric: Faster recorded plan average
metric: Slower recorded plan average
metric: Recorded slower-plan time
metric: Recorded slower-plan executions
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse.

Recorded run times for a read from orders: median 150 ms, 99th percentile 4.0 s · Low · Medium
Among the retained logged runs of this statement (long_query_time is currently 100 ms; historical thresholds, filters and coverage are not established): the recorded median is 150 ms and the 99th percentile is 4.0 s. 3 of 80 recorded histogram samples have bucket lower bounds at or above 1.5 s.
impact: Recorded statement workload
metric: Recorded statement time
metric: Recorded median
metric: Recorded 99th percentile
metric: Recorded histogram samples
metric: Samples in slow histogram buckets
fix: Compare representative fast and slow executions of this statement for equivalent input classes and work. Obtain available actual rows, access paths, waits and concurrent resource context. Establish the mechanism before changing parameterization, plan reuse, statistics or indexes.
verify: In an owned test environment, compare representative input classes under equivalent work and concurrency. Check latency, CPU, reads, waits and compilation costs against the predicted mechanism.
verify: Re-scan after an evidenced change and compare matching execution populations in Queries → Runtime spread; a faster average caused by a different input mix does not verify the fix.
caveat: Different input sizes or distributions, statistics or schema changes, cache state, blocking, I/O and concurrency can explain these differences. The captured evidence does not establish harmful parameter-sensitive plan reuse."###;
    crate::pins::pin_text("parameter-variation-followup", &followup.join("\n\n"), FOLLOWUP);
}

// ── plan-warnings: no join predicate ─────────────────────────────────────────────────────────────

fn without_join_predicate(text: &str) -> Finding {
    let mut s = snapshot("mssql-demo.json");
    let q = query(&mut s, "8");
    q.text = text.into();
    q.plan.as_mut().unwrap().root.warnings.push(PlanWarning::NoJoinPredicate);
    analyze(s).findings.into_iter().find(|f| f.id == "plan-warnings:8:no-join-predicate").expect("the warning")
}

#[test]
fn a_join_predicate_sql_server_dropped_is_not_a_missing_on() {
    let f = without_join_predicate(
        "SELECT o.id, c.name FROM shop.orders o JOIN shop.customers c ON c.id = o.customer_id WHERE o.customer_id = @P1 AND c.id = @P1",
    );
    assert_eq!(f.severity, Severity::Low);
    assert!(f.summary.contains("though the statement does join them"), "{}", f.summary);
    let f = without_join_predicate(
        "SELECT o.id, c.name FROM shop.orders o, shop.customers c WHERE o.customer_id = c.id AND c.id = @P1",
    );
    assert_eq!(f.severity, Severity::Low);
    let f = without_join_predicate("SELECT o.id, c.name FROM shop.orders o, shop.customers c WHERE c.id = @P1");
    assert!(f.title.ends_with("joins tables without a join condition"), "{}", f.title);
}

// ── query-family ─────────────────────────────────────────────────────────────────────────────────

fn family(sample: &str, ids: &[&str]) -> QueryFamily {
    QueryFamily {
        key: "0x8A1F".into(),
        sample: sample.into(),
        variants: 300.0,
        executions: 9000.0,
        total_time_ms: 5000.0,
        cached_bytes: None,
        query_ids: ids.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn copies_that_differ_in_parameter_declarations_need_sizes_not_parameterization() {
    let mut s = snapshot("mssql-demo.json");
    // Two of the members, same statement, string parameter sized from each value.
    let mut twin = query(&mut s, "7").clone();
    query(&mut s, "7").parameters = Some("@P1 nvarchar(7)".into());
    twin.id = "7001".into();
    twin.parameters = Some("@P1 nvarchar(12)".into());
    s.workload.as_mut().unwrap().queries.push(twin);
    s.query_families = vec![family(
        "(@P1 nvarchar(7))SELECT [id], [name], [segment] FROM [shop].[customers] WHERE [email] = @P1",
        &["7", "7001"],
    )];
    let f = analyze(s).findings.into_iter().find(|f| f.check == "query-family").expect("a family");
    assert!(f.title.contains("differ only in how their parameters are declared"), "{}", f.title);
    assert!(f.title.contains("a read from customers"), "{}", f.title);
    assert!(f.summary.contains("`@P1 nvarchar(7)` and `@P1 nvarchar(12)`"), "{}", f.summary);
    let fix = f.fix.unwrap();
    assert!(fix.sql.is_none() && fix.summary.contains("fixed size"), "{}", fix.summary);
}

#[test]
fn copies_with_the_same_text_are_no_literal_family() {
    // SQL Server: the same text and declaration under two query ids (another module, other SET options).
    let mut s = snapshot("mssql-demo.json");
    let mut twin = query(&mut s, "7").clone();
    twin.id = "7001".into();
    s.workload.as_mut().unwrap().queries.push(twin);
    s.query_families = vec![family("SELECT [id] FROM [shop].[customers] WHERE [email] = @P1", &["7", "7001"])];
    assert!(!analyze(s).findings.iter().any(|f| f.check == "query-family"));

    // PostgreSQL: one entry per role that ran the same statement, which has no value list to differ in.
    let mut pg = snapshot("postgres-demo.json");
    pg.query_families = vec![family("SELECT name, email FROM customers WHERE id = $1", &[])];
    assert!(!analyze(pg).findings.iter().any(|f| f.check == "query-family"));
}

#[test]
fn ysql_families_verify_new_entries_instead_of_promising_old_entries_disappear() {
    let mut s = snapshot("yugabyte-demo.json");
    s.query_families = vec![family("SELECT id FROM public.orders WHERE id IN ($1, $2)", &[])];
    let f = analyze(s).findings.into_iter().find(|f| f.check == "query-family").unwrap();
    assert!(f.verify[0].contains("keeps an entry for each copy"));
    assert!(f.verify[0].contains("no new copies should appear"));
}

#[test]
fn ysql_unstable_writes_describe_key_share_locks_and_cumulative_spreads() {
    let mut s = snapshot("yugabyte-demo.json");
    let (mean, sd, max, n) = spread_of(&[(2.0, 9900.0), (5000.0, 100.0)]);
    let w = s.workload.as_mut().unwrap();
    w.queries.truncate(1);
    let q = &mut w.queries[0];
    let id = q.id.clone();
    q.text = "INSERT INTO public.orders (id, customer_id) VALUES ($1, $2)".into();
    let mut p = insert_plan_into("public.orders");
    p.engine = culprit_core::Engine::Yugabyte;
    q.plan = Some(p);
    q.plans.clear();
    q.trend = None;
    q.executions = n;
    q.mean_time_ms = mean;
    q.stddev_time_ms = Some(sd);
    q.max_time_ms = Some(max);
    q.total_time_ms = n * mean;
    w.total_time_ms = q.total_time_ms;
    let report = analyze(s);
    let f = find(&report.findings, &format!("unstable-query:{id}")).unwrap();
    assert!(f.summary.contains("FOR KEY SHARE"), "{}", f.summary);
    assert!(f.summary.contains("deletes or changes the key of"), "{}", f.summary);
    assert!(f.caveats.iter().any(|c| c.contains("could be from any time")));
}

// ── tuning-advice: a forced plan that scans ──────────────────────────────────────────────────────

/// Automatic tuning forced plan 9 for the demo's query 9 on Oct 3; the plan scans order_items for an order id, and an
/// index leading with order_id exists. The scan keeps `kept` of the table's 1.5M rows.
fn forced_scan(kept: f64, include: &[&str], created_at_latest: Option<&str>) -> Option<Finding> {
    let mut s = snapshot("mssql-demo.json");
    s.tuning = Some(TuningAdvice {
        recommendations: vec![TuningRecommendation {
            name: "PR_9".into(),
            kind: "FORCE_LAST_GOOD_PLAN".into(),
            state: Some("Verifying".into()),
            query_id: Some("9".into()),
            recommended_plan_id: Some("9".into()),
            applied_at: Some("2026-10-03T08:00:00Z".into()),
            ..Default::default()
        }],
        forced_plans: vec![ForcedPlan {
            query_id: "9".into(),
            plan_id: "9".into(),
            forcing: Some("AUTO".into()),
            ..Default::default()
        }],
        ..Default::default()
    });
    let mut index = s.indexes.iter().find(|i| i.name == "ix_order_items_product_id").unwrap().clone();
    index.name = "ix_order_items_order_id".into();
    index.columns = vec!["order_id".into()];
    index.include = include.iter().map(|c| c.to_string()).collect();
    index.created_at_latest = created_at_latest.map(str::to_string);
    s.indexes.push(index);
    let scan = &mut query(&mut s, "9").plan.as_mut().unwrap().root.children[0];
    scan.est_rows = 1_500_000.0 * kept;
    analyze(s).findings.into_iter().find(|f| f.id == "tuning-advice:forced-scan:9")
}

#[test]
fn a_forced_scan_is_flagged_only_where_a_seek_would_read_less() {
    // 1.8 rows of 1.5M: a seek and a lookup per row beats the scan, covering index or not.
    let f = forced_scan(1.8 / 1_500_000.0, &[], None).expect("a selective filter");
    assert!(f.summary.contains("doesn't hold product_id, quantity and unit_price"), "{}", f.summary);
    assert!(!f.summary.contains("forced before it existed"), "nothing dates the index: {}", f.summary);
    // Keeping 70% of the table, the scan is the right plan.
    assert!(forced_scan(0.7, &["product_id", "quantity", "unit_price"], None).is_none());
    // Keeping 5%: only an index that holds every column it returns beats the scan.
    assert!(forced_scan(0.05, &[], None).is_none());
    let f = forced_scan(0.05, &["product_id", "quantity", "unit_price"], None).expect("a covering index");
    assert!(!f.summary.contains("doesn't hold"), "{}", f.summary);
}

#[test]
fn a_forced_plan_is_called_older_than_its_index_only_when_the_index_is_dated() {
    let f = forced_scan(1e-6, &[], Some("2026-10-09T12:00:00Z")).unwrap();
    assert!(f.summary.contains("so this plan was most likely forced before it existed"), "{}", f.summary);
    // Dated before the forcing: the index was already there.
    let f = forced_scan(1e-6, &[], Some("2026-10-01T12:00:00Z")).unwrap();
    assert!(!f.summary.contains("forced before it existed"), "{}", f.summary);
}

#[test]
fn statistics_maintenance_is_not_told_to_batch_application_updates() {
    let mut s = snapshot("mssql-demo.json");
    let w = s.workload.as_mut().unwrap();
    let mut q = w.queries[0].clone();
    q.id = "statistics-maintenance".into();
    q.text = "UPDATE STATISTICS dbo.orders WITH FULLSCAN".into();
    q.object_name = None;
    q.plan = None;
    q.plans.clear();
    q.waits.clear();
    q.trend = None;
    q.executions = 2.0;
    q.total_time_ms = 20_000.0;
    q.mean_time_ms = 10_000.0;
    q.rows = None;
    q.first_seen = None;
    q.last_seen = None;
    w.total_time_ms = q.total_time_ms;
    w.total_executions = q.executions;
    w.queries = vec![q];
    let findings = heavy_queries(&s);
    let f = find(&findings, "heavy-query:statistics-maintenance").unwrap();
    assert!(f.title.starts_with("A statistics refresh on orders"), "{}", f.title);
    assert!(f.summary.contains("optimizer statistics maintenance"), "{}", f.summary);
    let fix = f.fix.as_ref().unwrap();
    assert!(fix.summary.contains("scheduled maintenance"));
    assert!(fix.sql.is_none());
    assert!(!f.verify.iter().any(|v| v.contains("pages read")));
}

#[test]
fn heavy_counter_advice_preserves_compound_updates_on_all_engines_and_windows() {
    use culprit_core::Engine;
    for (engine, sql) in [
        (
            Engine::Mssql,
            "UPDATE shop.counters SET n = n + 1, updated_at = SYSUTCDATETIME() OUTPUT INSERTED.n WHERE id = @id",
        ),
        (Engine::Postgres, "UPDATE shop.counters SET n = n + 1, updated_at = now() WHERE id = $1 RETURNING n"),
        (Engine::Mysql, "UPDATE shop.counters SET n = LAST_INSERT_ID(n + 1), updated_at = NOW() WHERE id = ?"),
        (Engine::Sqlite, "UPDATE counters SET n = n + 1, updated_at = CURRENT_TIMESTAMP WHERE id = ? RETURNING n"),
    ] {
        for hours in [1.0, 24.0, 168.0, 720.0] {
            let mut s = snapshot("mssql-demo.json");
            s.server.engine = engine;
            let w = s.workload.as_mut().unwrap();
            let mut q = w.queries[0].clone();
            q.text = sql.into();
            q.id = "compound-counter".into();
            q.object_name = None;
            q.plan = None;
            q.plans.clear();
            q.waits.clear();
            q.trend = None;
            q.first_seen = None;
            q.last_seen = None;
            q.executions = 100_000.0;
            q.mean_time_ms = 1.0;
            q.total_time_ms = 100_000.0;
            q.rows = Some(q.executions);
            q.logical_reads = Some(3.0 * q.executions);
            w.windowed = true;
            w.window_hours = hours;
            w.hourly_time_ms = None;
            w.total_time_ms = q.total_time_ms;
            w.total_executions = q.executions;
            w.queries = vec![q];
            let found = heavy_queries(&s);
            let f = found.iter().find(|f| f.id == "heavy-query:compound-counter").unwrap();
            let fix = &f.fix.as_ref().unwrap().summary;
            assert!(
                fix.contains("preserve every assignment, returned value, row filter and transaction guarantee"),
                "{engine:?}/{hours}: {fix}"
            );
            assert!(f.verify.iter().any(|v| v.contains("numbering") && v.contains("metadata")), "{:?}", f.verify);
        }
    }
}

#[test]
fn frequent_empty_results_do_not_establish_a_timer_or_multiple_pollers() {
    let mut s = snapshot("mssql-demo.json");
    let w = s.workload.as_mut().unwrap();
    let mut q = w.queries[0].clone();
    q.id = "request-check".into();
    q.text = "SELECT id FROM dbo.orders WHERE token = @token".into();
    q.object_name = None;
    q.plan = None;
    q.plans.clear();
    q.waits.clear();
    q.trend = None;
    q.executions = 100_000.0;
    q.total_time_ms = 10_000.0;
    q.mean_time_ms = 0.1;
    q.rows = Some(0.0);
    q.first_seen = None;
    q.last_seen = None;
    w.total_time_ms = q.total_time_ms;
    w.total_executions = q.executions;
    crate::last_hours(w, 24.0);
    w.queries = vec![q];
    let report = analyze(s);
    let f = find(&report.findings, "chatty-query:request-check").unwrap();
    assert!(f.summary.contains("request-driven checks"), "{}", f.summary);
    assert!(!f.summary.contains("the app is polling") && !f.summary.contains("every instance"));
    assert!(f.fix.as_ref().unwrap().summary.contains("Check the caller first"));
}

#[test]
fn a_single_recovered_plan_verifies_against_the_retained_baseline() {
    use culprit_core::{SourceState, VerifyStatus};
    let before = analyze(with_plans(vec![
        plan("8", 200.0, 9.0, "2026-10-11T07:44:00Z", "2026-10-11T10:44:00Z"),
        plan("47", 100.0, 60.0, "2026-10-11T10:46:00Z", "2026-10-11T12:13:00Z"),
    ]));
    let finding = find(&before.findings, "plan-regression:8").unwrap();
    let previous = culprit_checks::digest(&before, None, 100, 0);
    // One current plan, forced or naturally selected, needs no second plan in the new window.
    for forced in [false, true] {
        let mut s = with_plans(vec![plan("8", 100.0, 10.0, "2026-10-11T12:14:00Z", "2026-10-11T12:30:00Z")]);
        s.taken_at = "2026-10-11T12:43:47Z".into();
        s.workload.as_mut().unwrap().window_start = Some("2026-10-11T08:10:00Z".into());
        s.workload.as_mut().unwrap().window_end = Some(s.taken_at.clone());
        query(&mut s, "8").plans[0].is_forced = forced;
        let verify = |after: &culprit_core::Report| {
            culprit_core::compare_reports(&before, after, std::slice::from_ref(&finding.id)).findings[0].status
        };
        let after = analyze(s.clone());
        assert_eq!(verify(&after), VerifyStatus::Fixed);
        for changes in [
            culprit_checks::digest(&after, Some(&before), 100, 0).since.unwrap(),
            culprit_checks::digest_since(&after, &previous, 100, 0).since.unwrap(),
        ] {
            assert!(changes.fixed.iter().any(|f| f.id == finding.id));
        }
        let assert_unknown = |s: Snapshot| {
            let after = analyze(s);
            assert_eq!(verify(&after), VerifyStatus::Unknown);
            assert!(
                !culprit_checks::digest_since(&after, &previous, 100, 0)
                    .since
                    .unwrap()
                    .fixed
                    .iter()
                    .any(|f| f.id == finding.id)
            );
        };
        let mut absorbed = after.clone();
        absorbed.refinement.push(culprit_core::RefinementDecision {
            rule: "another cause explains this observation".into(),
            finding: Some(finding.id.clone()),
            outcome: culprit_core::RefinementOutcome::Absorbed { into: "another-finding".into() },
            queries: vec![],
            tables: vec![],
        });
        assert_eq!(verify(&absorbed), VerifyStatus::Unknown);
        let mut slow = s.clone();
        query(&mut slow, "8").plans[0].mean_time_ms = 60.0;
        assert_unknown(slow);
        let mut sparse = s.clone();
        query(&mut sparse, "8").plans[0].executions = 1.0;
        assert_unknown(sparse);
        let mut historical = s.clone();
        query(&mut historical, "8").plans[0].first_seen = Some("2026-10-11T07:44:00Z".into());
        assert_unknown(historical);
        let mut failed = s.clone();
        crate::record(
            &mut failed,
            "query_store",
            SourceState::Partial,
            Some("Some runtime intervals could not be read"),
        );
        assert_unknown(failed);
        let mut future = s.clone();
        query(&mut future, "8").plans[0].last_seen = Some("2026-10-12T12:30:00Z".into());
        assert_unknown(future);
        let mut missing = s.clone();
        query(&mut missing, "8").plans.clear();
        assert_unknown(missing);
    }
}
