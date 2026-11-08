//! The `plan-regression` check: Queries whose current plan is much slower than a plan they used before.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::t;

pub fn plan_regression_info() -> CheckInfo {
    CheckInfo {
        id: "plan-regression".into(),
        title: t!("check.plan_regression.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql],
        requires: vec![Capability::QueryStore],
        description: t!("check.plan_regression.info.description"),
    }
}

check!(
    PlanRegressionCheck: plan_regression_info,
    decide = plan_regressions,
    render = plan_regression_finding,
    case<'s> = PlanRegression<'s>,
    evidence = &[Evidence::Query(QueryMeasure::PlanVariants)],
);

/// A plan with this many runs or more counts as one the query used.
pub(super) const PLAN_MIN_RUNS: f64 = culprit_core::PLAN_VARIANT_MIN_RUNS;
/// The faster plan is the baseline only over this many runs, the bar `regression` sets for one: five quick runs in nine
/// minutes are a moment, not how the query ran.
const BASELINE_MIN_RUNS: f64 = 20.0;
/// A slower plan does more work per run than the one before by this factor, in CPU or in reads, where the source keeps
/// them per plan: otherwise its runs only waited longer (blocking, load, a busier time of day), which a plan change
/// doesn't explain and forcing the old plan doesn't fix.
const MORE_WORK: f64 = 1.5;

/// Whether `latest` does more work per run than `best`: `None` when the source doesn't keep it per plan.
pub(super) fn does_more_work(best: &PlanVariant, latest: &PlanVariant) -> Option<bool> {
    let per_run = |total: Option<f64>, p: &PlanVariant| total.map(|t| t / p.executions.max(1.0));
    let cpu = per_run(latest.cpu_time_ms, latest).zip(per_run(best.cpu_time_ms, best));
    let reads = per_run(latest.logical_reads, latest).zip(per_run(best.logical_reads, best));
    if cpu.is_none() && reads.is_none() {
        return None;
    }
    let worse = |(now, then): (f64, f64)| now >= then.max(0.001) * MORE_WORK;
    Some(cpu.is_some_and(worse) || reads.is_some_and(worse))
}

/// When a plan ran, from its first to its last run (epoch seconds).
pub(super) fn plan_span(p: &PlanVariant) -> Option<(f64, f64)> {
    Some((parse_epoch_seconds(p.first_seen.as_deref()?)?, parse_epoch_seconds(p.last_seen.as_deref()?)?))
}

/// The plan a query runs with now, much slower than one it ran with before and stopped using. Both claims need times:
/// the fast plan's last run comes before the slow plan's first, and no other plan ran alongside the slow one (plans
/// used side by side for different values are parameter sensitivity, not a plan that changed).
pub(super) fn replaced_plan(q: &QueryStat) -> Option<(&PlanVariant, &PlanVariant)> {
    let usable: Vec<&PlanVariant> = q.workload_plans().iter().filter(|p| p.executions >= PLAN_MIN_RUNS).collect();
    if usable.len() < 2 {
        return None;
    }
    let spans: Vec<(f64, f64)> = usable.iter().map(|p| plan_span(p)).collect::<Option<_>>()?;
    let (latest_at, (latest_first, latest_last)) =
        spans.iter().copied().enumerate().max_by(|a, b| a.1.1.total_cmp(&b.1.1))?;
    let latest = usable[latest_at];
    let overlaps = spans
        .iter()
        .enumerate()
        .any(|(i, (first, last))| i != latest_at && *first <= latest_last && latest_first <= *last);
    if overlaps || latest.is_forced {
        return None;
    }
    let best = usable
        .iter()
        .zip(&spans)
        .filter(|(p, (_, last))| *last < latest_first && p.executions >= BASELINE_MIN_RUNS)
        .map(|(p, _)| *p)
        .min_by(|a, b| a.mean_time_ms.total_cmp(&b.mean_time_ms))?;
    (!best.is_forced
        && latest.mean_time_ms >= best.mean_time_ms * 2.0
        && latest.mean_time_ms - best.mean_time_ms >= 5.0
        && does_more_work(best, latest) != Some(false))
    .then_some((best, latest))
}

/// A statement whose plan changed for a much slower one, and what that establishes.
pub struct PlanRegression<'s> {
    query: &'s QueryStat,
    impact: QueryImpact,
    /// The plan it ran fast with, and the slower one that replaced it.
    best: &'s PlanVariant,
    latest: &'s PlanVariant,
    /// How many times slower the replacement is.
    factor: f64,
    /// The slow plan's runs' time beyond the fast plan's (ms).
    wasted: f64,
    /// That a day, over a day or more of the slow plan's runs.
    wasted_per_day: Option<f64>,
    score: f64,
    severity: Severity,
    /// It stopped running: the slow plan ran for a while, and was its last.
    stopped: bool,
    /// A table its plan reads that the catalog no longer has: there is nothing to force.
    gone: Option<String>,
}

/// The statements whose plan was replaced by a much slower one that is a real part of the workload.
fn plan_regressions(snapshot: &Snapshot) -> Vec<PlanRegression<'_>> {
    let Some(workload) = snapshot.workload.as_ref() else { return Vec::new() };
    app_queries(snapshot)
        .filter_map(|q| {
            let (best, latest) = replaced_plan(q)?;
            let impact = query_impact(snapshot, &[q])?;
            let factor = latest.mean_time_ms / best.mean_time_ms.max(0.001);
            let wasted = (latest.mean_time_ms - best.mean_time_ms) * latest.executions;
            // The same bar as `regression`: the slow plan's extra time, or its own time, is a real part of the workload.
            let (wasted_share, latest_share) =
                (workload.share_of(wasted, q), workload.share_of(latest.total_time_ms, q));
            if wasted_share < 0.01 && latest_share < 0.03 {
                return None;
            }
            // Its time a day since it took over, so a regression that began this morning isn't diluted by the week.
            let since = latest.first_seen.as_deref().and_then(parse_epoch_seconds);
            let end = workload.window_end.as_deref().or(Some(snapshot.taken_at.as_str())).and_then(parse_epoch_seconds);
            // The extra time a day over the hours the slow plan ran, from its first run to its last (to the window's end
            // while it still runs), and only over a day of them: a burst of three minutes is no rate a day.
            let stopped = crate::query_stopped(snapshot, q).is_some();
            let until = if stopped { latest.last_seen.as_deref().and_then(parse_epoch_seconds) } else { end };
            let wasted_per_day = match (since, until) {
                (Some(since), Some(until)) if (until - since) / 3600.0 >= culprit_core::PER_DAY_MIN_HOURS => {
                    Some(wasted * 86_400.0 / (until - since))
                }
                _ => None,
            };
            let amount = match (since, end) {
                // Runs that came in bursts are graded on their total, never extrapolated to a day.
                (Some(since), Some(end)) => severity_for_recent_time(
                    burst_hours(snapshot, q, (end - since) / 3600.0),
                    latest.total_time_ms,
                    1.0,
                ),
                _ => severity_for_time_of(snapshot, &[q], latest.total_time_ms, 1.0),
            };
            let mut severity = severity_for_share(latest_share).min(amount);
            if factor >= 3.0 && wasted_share >= 0.05 {
                severity = severity.max(Severity::Medium.min(amount));
            }
            Some(PlanRegression {
                query: q,
                impact,
                best,
                latest,
                factor,
                wasted,
                wasted_per_day,
                score: score_for_share(latest_share.max(wasted_share)),
                severity,
                stopped,
                gone: table_gone(snapshot, q),
            })
        })
        .collect()
}

fn plan_regression_finding(snapshot: &Snapshot, regression: PlanRegression<'_>) -> Finding {
    let PlanRegression { query: q, best, latest, .. } = regression;
    let mut f = finding("plan-regression", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    f.facts.plan_change = Some(culprit_core::FindingPlanChange { from: best.id.clone(), to: latest.id.clone() });
    f.facts.plan_baseline = latest.last_seen.as_ref().map(|last| culprit_core::PlanBaseline {
        query: q.id.clone(),
        mean_ms: best.mean_time_ms,
        observed_until: last.clone(),
    });
    apply_impact(snapshot, &mut f, &regression.impact);
    f.impact.time_ms_per_day = regression.wasted_per_day;
    f.impact.score = regression.score;
    let factor = format::factor(regression.factor);
    f.impact.set_label(t!("common.impact.times_slower", factor = factor.clone()));
    f.severity = regression.severity;
    f.confidence = Confidence::High;
    f.title = t!("check.plan_regression.title", query = capitalize(&query_handle(q)), factor = factor);
    f.summary = plan_change_sentence(&regression, None);
    f.queries = query_refs(&[q]);
    f.metrics.push(Metric::said(culprit_core::key!("common.metric.fast_plan"), best.mean_time_ms, Unit::Ms));
    // A statement that stopped has no current plan: the slow one is the last it ran with.
    let slow_label = if regression.stopped || regression.gone.is_some() {
        t!("check.plan_regression.metric.last_plan")
    } else {
        t!("check.plan_regression.metric.current_plan")
    };
    f.metrics.push(Metric::new(slow_label, latest.mean_time_ms, Unit::Ms));
    match regression.wasted_per_day {
        Some(extra) => {
            f.metrics.push(Metric::said(culprit_core::key!("common.metric.extra_time_per_day"), extra, Unit::Ms))
        }
        None => {
            f.metrics.push(Metric::said(culprit_core::key!("common.metric.extra_time"), regression.wasted, Unit::Ms))
        }
    }
    // A plan forced onto a table that is gone can't apply: there is nothing to force.
    if let Some(gone) = regression.gone {
        f.actionability = culprit_core::Actionability::History;
        f.fix = Some(Fix {
            summary: t!("check.plan_regression.fix.table_gone", table = gone, plan = &best.id),
            sql: None,
            cost: None,
        });
        f.verify = vec![t!("check.plan_regression.verify.table_gone")];
        f.facts.nothing_to_check = Some(culprit_core::NothingToCheck::UntilItReturns);
        return f;
    }
    // Where SQL Server corrects regressions itself, the fix says what it does and what forcing by hand adds.
    let mut fix = vec![t!("check.plan_regression.fix.force")];
    if let Some(note) = crate::tuning::hand_forcing_note(snapshot) {
        fix.push(note);
        fix.push(t!("check.plan_regression.fix.not_forced_yet", plan = &best.id));
    }
    f.fix = Some(Fix {
        summary: fix.join(" "),
        sql: Some(format!("EXEC sp_query_store_force_plan @query_id = {}, @plan_id = {};", q.id, best.id)),
        cost: None,
    });
    f.verify = vec![t!("check.plan_regression.verify.force")];
    f.caveats.push(t!("check.plan_regression.caveat.force_failure"));
    f
}

/// The plan change, as its own finding says it, or (`told_elsewhere`) as a finding that takes it in says it: of "this
/// query", or (`Some(named)`) naming it where that finding is about several.
fn plan_change_sentence(regression: &PlanRegression<'_>, told_elsewhere: Option<bool>) -> String {
    let PlanRegression { query: q, best, latest, .. } = regression;
    let when = |at: Option<&str>| at.map(crate::queries::short_when).unwrap_or_default();
    let (fast, fast_ms, until) = (&best.id, format::duration(best.mean_time_ms), when(best.last_seen.as_deref()));
    let (slow, slow_ms, runs) = (&latest.id, format::duration(latest.mean_time_ms), format::count(latest.executions));
    let named = if told_elsewhere == Some(true) { "yes" } else { "no" };
    match (regression.stopped, latest.first_seen.as_deref(), latest.last_seen.as_deref()) {
        // A statement that stopped ran the slow plan for a while, not "since".
        (true, Some(first), Some(last)) => {
            let span = culprit_core::span_when(first, last);
            match told_elsewhere {
                None => t!(
                    "check.plan_regression.summary.stopped",
                    fast = fast,
                    fast_ms = fast_ms,
                    until = until,
                    slow = slow,
                    slow_ms = slow_ms,
                    span = span,
                    runs = runs
                ),
                Some(_) => t!(
                    "check.plan_regression.timeline.stopped",
                    named = named,
                    query = &q.id,
                    fast = fast,
                    fast_ms = fast_ms,
                    until = until,
                    slow = slow,
                    slow_ms = slow_ms,
                    span = span,
                    runs = runs
                ),
            }
        }
        _ => {
            let since = when(latest.first_seen.as_deref());
            match told_elsewhere {
                None => t!(
                    "check.plan_regression.summary.running",
                    fast = fast,
                    fast_ms = fast_ms,
                    until = until,
                    slow = slow,
                    slow_ms = slow_ms,
                    since = since,
                    runs = runs
                ),
                Some(_) => t!(
                    "check.plan_regression.timeline.running",
                    named = named,
                    query = &q.id,
                    fast = fast,
                    fast_ms = fast_ms,
                    until = until,
                    slow = slow,
                    slow_ms = slow_ms,
                    since = since,
                    runs = runs
                ),
            }
        }
    }
}

/// The plan change of `query` as a finding that takes it in says it, naming the statement where that finding is about
/// several (`named`), where this scan shows one.
pub(crate) fn plan_regression_timeline(snapshot: &Snapshot, query: &str, named: bool) -> Option<String> {
    let regression = plan_regressions(snapshot).into_iter().find(|r| r.query.id == query)?;
    Some(plan_change_sentence(&regression, Some(named)))
}

/// A table the statement's plan reads that the catalog no longer has (dropped or renamed since), by its name as the
/// plan writes it. `None` when the scan read no tables at all (the catalog wasn't readable), for temporary tables and
/// table variables, and for a name the plan doesn't qualify with its schema.
fn table_gone(snapshot: &Snapshot, q: &QueryStat) -> Option<String> {
    if snapshot.tables.is_empty() {
        return None;
    }
    q.plan
        .as_ref()?
        .nodes()
        .into_iter()
        .filter(|n| matches!(n.kind, NodeKind::Scan | NodeKind::Seek | NodeKind::Lookup))
        .find_map(|n| {
            let object = n.object.as_deref().filter(|o| o.contains('.'))?;
            let (schema, table) = crate::indexes::split_object(object)?;
            if table.starts_with(['#', '@']) {
                return None;
            }
            snapshot.table(&schema, &table).is_none().then(|| format!("{schema}.{table}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SQL Server demo's query 8, its plan 8 replaced by a 6.7× slower plan 47, and no timeline.
    fn replaced() -> Snapshot {
        let mut s: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
        let w = s.workload.as_mut().unwrap();
        let q = w.queries.iter_mut().find(|q| q.id == "8").unwrap();
        q.plans = [
            ("8", 200.0, 9.0, "2026-10-11T07:44:00Z", "2026-10-11T10:44:00Z"),
            ("47", 100.0, 60.0, "2026-10-11T10:46:00Z", "2026-10-11T12:13:00Z"),
        ]
        .map(|(id, runs, mean, first, last)| {
            serde_json::from_value(serde_json::json!({
                "id": id, "executions": runs, "total_time_ms": runs * mean, "mean_time_ms": mean,
                "first_seen": first, "last_seen": last, "is_forced": false
            }))
            .unwrap()
        })
        .to_vec();
        q.executions = 300.0;
        q.total_time_ms = 200.0 * 9.0 + 100.0 * 60.0;
        q.mean_time_ms = q.total_time_ms / q.executions;
        q.trend = None;
        w.total_time_ms = w.queries.iter().map(|q| q.total_time_ms).sum();
        s
    }

    // Another finding tells the plan change from its facts: of this query, or naming it among several.
    #[test]
    fn another_finding_tells_the_plan_change_from_its_facts() {
        let report = crate::analyze(replaced());
        let f = report.findings.iter().find(|f| f.id == "plan-regression:8").expect("plan 47 replaced plan 8");
        let this = plan_regression_timeline(&report.snapshot, "8", false).unwrap();
        assert!(this.starts_with("Query Store saw this query run with plan 8 at 9 ms"), "{this}");
        assert!(f.summary.contains(&this), "{}", f.summary);
        let named = plan_regression_timeline(&report.snapshot, "8", true).unwrap();
        assert_eq!(named, this.replacen("this query", "query 8", 1));
        let french = culprit_core::i18n::scope("fr", || plan_regression_timeline(&report.snapshot, "8", true)).unwrap();
        assert!(french.starts_with("Query Store a vu la requête 8 s’exécuter avec le plan 8"), "{french}");
    }
}
