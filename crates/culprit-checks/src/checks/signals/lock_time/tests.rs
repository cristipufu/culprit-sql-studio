use super::*;
use culprit_core::{QueryWaitCoverage, Spread, Timeline, TimelineSource, WaitStat};

fn capture(recorded_ms: f64) -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    let mut query = crate::support::tests::sample();
    query.text = "UPDATE inventory SET quantity = quantity - 1 WHERE id = @id".into();
    query.executions = 2.0;
    query.total_time_ms = 10_000.0;
    query.mean_time_ms = 5_000.0;
    query.max_time_ms = Some(9_000.0);
    query.waits = vec![WaitStat {
        category: WaitClass::Lock.category().into(),
        wait_type: Some("Native.SchemaLock".into()),
        wait_ms: recorded_ms,
        tasks: None,
    }];
    snapshot.taken_at = "2026-11-13T00:00:00Z".into();
    let workload = snapshot.workload.as_mut().unwrap();
    workload.queries = vec![query];
    workload.total_time_ms = 10_000.0;
    workload.total_executions = 2.0;
    workload.window_hours = 24.0;
    workload.window_start = Some("2026-11-12T00:00:00Z".into());
    workload.window_end = Some(snapshot.taken_at.clone());
    workload.timeline = None;
    snapshot
}

#[test]
fn parallel_lock_totals_do_not_establish_elapsed_shares_or_a_longest_run() {
    let snapshot = capture(40_000.0);
    let findings = LockTimeCheck.run(&snapshot);
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    assert_eq!(finding.facts.lock_wait.as_ref().unwrap().ms, 40_000.0);
    assert_eq!(finding.impact.db_time_share, None);
    assert_eq!(finding.impact.time_ms_per_day, None);
    assert!(finding.metrics.iter().all(|metric| !matches!(metric.unit, Unit::Ratio)));
    let mean = finding.metrics.iter().find(|metric| metric.is("check.lock_time.metric.per_run")).unwrap();
    assert_eq!(mean.value, 20_000.0, "a mean of accumulated waits is not clamped to run duration");
    assert!(!finding.title.contains('%') && !finding.title.contains("one run waited"), "{}", finding.title);
    assert!(finding.title.contains(&format::duration(40_000.0)), "{}", finding.title);
    assert!(finding.summary.contains("do not identify the lock resource or blocker"), "{}", finding.summary);
    let fix = &finding.fix.as_ref().unwrap().summary;
    assert!(fix.contains("Identify the lock resource, mode and blocking request"), "{fix}");
    assert!(!fix.contains("won't help") && !fix.contains("holding the rows will"), "{fix}");
}

#[test]
fn partial_positive_lock_evidence_never_becomes_a_measured_absence() {
    for complete in [false, true] {
        let mut snapshot = capture(40_000.0);
        snapshot.workload.as_mut().unwrap().queries[0].waits_coverage =
            Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete });
        let cases = lock_waits(&snapshot);
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].recorded_ms, 40_000.0);
        assert_eq!(cases[0].complete, complete);
    }
    let mut snapshot = capture(40_000.0);
    let query = &mut snapshot.workload.as_mut().unwrap().queries[0];
    query.waits.clear();
    query.waits_coverage = Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: false });
    assert!(lock_waits(&snapshot).is_empty());
    query_mut(&mut snapshot).waits_coverage.as_mut().unwrap().complete = true;
    assert!(lock_waits(&snapshot).is_empty(), "qualified zero is not a positive lock-wait finding");
}

fn query_mut(snapshot: &mut Snapshot) -> &mut QueryStat {
    &mut snapshot.workload.as_mut().unwrap().queries[0]
}

#[test]
fn invalid_and_overflowing_lock_totals_are_not_reported() {
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0] {
        assert!(lock_waits(&capture(invalid)).is_empty(), "{invalid}");
    }
    let mut snapshot = capture(f64::MAX);
    let query = query_mut(&mut snapshot);
    query.waits.push(query.waits[0].clone());
    assert!(lock_waits(&snapshot).is_empty(), "sum overflow is unknown, not a maximum-sized finding");
}

#[test]
fn native_scalar_lock_evidence_is_not_selected_by_engine_identity_or_counted_twice() {
    for engine in Engine::ALL {
        let mut snapshot = capture(40_000.0);
        snapshot.server.engine = engine;
        query_mut(&mut snapshot).spread = Some(Spread { lock_ms: Some(80_000.0), ..Default::default() });
        assert_eq!(lock_waits(&snapshot)[0].recorded_ms, 40_000.0, "native class and scalar are not added");
        query_mut(&mut snapshot).waits.clear();
        assert_eq!(lock_waits(&snapshot)[0].recorded_ms, 80_000.0, "explicit timer remains native positive evidence");
    }
}

#[test]
fn invalid_execution_counts_do_not_invent_a_mean_or_erase_the_recorded_total() {
    for executions in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut snapshot = capture(40_000.0);
        query_mut(&mut snapshot).executions = executions;
        let cases = lock_waits(&snapshot);
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].recorded_ms, 40_000.0);
        assert_eq!(cases[0].per_execution_ms, None);
    }
}

#[test]
fn unknown_or_short_native_period_cannot_promote_a_large_wait_total() {
    for hours in [0.0, 0.5] {
        let mut snapshot = capture(3_600_000.0);
        let workload = snapshot.workload.as_mut().unwrap();
        workload.window_start = None;
        workload.window_end = None;
        workload.window_hours = hours;
        assert_eq!(lock_waits(&snapshot)[0].severity, Severity::Low);
        assert!(lock_waits(&snapshot)[0].no_rate);
    }
}

#[test]
fn recent_execution_cadence_cannot_turn_lifetime_lock_totals_into_current_wait_rates() {
    let mut snapshot = capture(600_000.0);
    let workload = snapshot.workload.as_mut().unwrap();
    workload.kind = Some(culprit_core::WorkloadKind::PerformanceSchema);
    workload.windowed = false;
    workload.window_start = Some("2026-11-03T00:00:00Z".into());
    workload.window_hours = 240.0;
    let query = &mut workload.queries[0];
    query.executions = 2_000.0;
    query.trend = Some(culprit_core::QueryTrend {
        time_ms: vec![Some(1000.0), Some(1000.0)],
        executions: vec![Some(500.0), Some(500.0)],
    });
    workload.timeline = Some(Timeline {
        source: TimelineSource::Scans,
        starts: vec!["2026-11-12T00:00:00Z".into(), "2026-11-12T12:00:00Z".into()],
        end: "2026-11-13T00:00:00Z".into(),
        time_ms: vec![1000.0, 1000.0],
        executions: vec![500.0, 500.0],
        unknown: None,
    });
    let query = &workload.queries[0];
    assert_eq!(workload.rate_per_day(600_000.0), Some(60_000.0));
    assert!(
        workload.rate_per_day_of(query, 600_000.0).unwrap() >= LOCK_MEDIUM_MS_PER_DAY,
        "rescaling historical waits by recent calls would cross the promotion threshold"
    );
    let cases = lock_waits(&snapshot);
    assert_eq!(cases[0].severity, Severity::Low, "ten native minutes over ten days are one minute a day");
    let finding = LockTimeCheck.run(&snapshot).pop().unwrap();
    assert!(finding.summary.contains("10 days"), "{}", finding.summary);
    assert_eq!(
        finding.metrics.iter().find(|metric| metric.is("check.lock_time.metric.lock_wait")).unwrap().value,
        600_000.0
    );
}
