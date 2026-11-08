//! Checks over the workload timeline: queries that got slower at a point in time.

use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::{
    Category, CheckInfo, Confidence, Engine, Finding, Fix, Metric, PlanVariant, QueryStat, QueryTrend, Severity,
    Snapshot, SubjectKind, Timeline, TimelineSource, Unit, format, key, t,
};

use crate::queries::{apply_impact, time_noun};
use crate::support::*;

/// "Much slower": the average run takes at least twice as long as before, and a few milliseconds more.
const MIN_FACTOR: f64 = 2.0;
const MIN_EXTRA_MS: f64 = 2.0;
/// A bucket counts as slow at 1.5× the old average; most buckets after the change must be slow, few before it.
const SLOW_BUCKET: f64 = 1.5;
const MIN_RECENT_SLOW: f64 = 0.7;
const MAX_BASELINE_SLOW: f64 = 0.3;
const MIN_BASELINE_RUNS: f64 = 20.0;
const MIN_RECENT_RUNS: f64 = 10.0;

pub fn regression_info() -> CheckInfo {
    CheckInfo {
        id: "regression".into(),
        title: t!("check.regression.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql],
        requires: vec![],
        description: t!("check.regression.info.description"),
    }
}

check!(
    RegressionCheck: regression_info,
    decide = regressions,
    render = regression_finding,
    case<'s> = Regression<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Timeline)],
);

/// Where a query's average duration stepped up and stayed up.
#[derive(Debug, PartialEq)]
pub(crate) struct Shift {
    /// First bucket of the slow period.
    pub start: usize,
    pub before_ms: f64,
    pub after_ms: f64,
    pub after_runs: f64,
    pub after_time_ms: f64,
}

impl Shift {
    fn factor(&self) -> f64 {
        self.after_ms / self.before_ms.max(0.001)
    }

    fn extra_ms(&self) -> f64 {
        (self.after_ms - self.before_ms) * self.after_runs
    }
}

/// The split of the timeline into a normal "before" and a slow "after" that explains the most extra time.
pub(crate) fn shift(trend: &QueryTrend, buckets: usize) -> Option<Shift> {
    let known: Vec<Option<(f64, f64)>> = (0..buckets)
        .map(|i| {
            let time = (*trend.time_ms.get(i)?)?;
            let runs = (*trend.executions.get(i)?)?;
            (runs > 0.0).then_some((time, runs))
        })
        .collect();
    let totals = |range: std::ops::Range<usize>| {
        known[range].iter().flatten().fold((0.0, 0.0, 0usize), |(t, r, n), (bt, br)| (t + bt, r + br, n + 1))
    };
    let mut best: Option<Shift> = None;
    for start in 2..buckets {
        let (before_time, before_runs, before_buckets) = totals(0..start);
        let (after_time, after_runs, after_buckets) = totals(start..buckets);
        if before_buckets < 2 || after_buckets < 2 || before_runs < MIN_BASELINE_RUNS || after_runs < MIN_RECENT_RUNS {
            continue;
        }
        let before_ms = before_time / before_runs;
        let after_ms = after_time / after_runs;
        if after_ms < before_ms * MIN_FACTOR || after_ms - before_ms < MIN_EXTRA_MS {
            continue;
        }
        let slow = |b: &Option<(f64, f64)>| b.is_some_and(|(t, r)| t / r >= before_ms * SLOW_BUCKET);
        if !slow(&known[start]) {
            continue;
        }
        let slow_after = known[start..].iter().filter(|b| slow(b)).count() as f64 / after_buckets as f64;
        let slow_before = known[..start].iter().filter(|b| slow(b)).count() as f64 / before_buckets as f64;
        if slow_after < MIN_RECENT_SLOW || slow_before > MAX_BASELINE_SLOW {
            continue;
        }
        let candidate = Shift { start, before_ms, after_ms, after_runs, after_time_ms: after_time };
        if best.as_ref().is_none_or(|b| candidate.extra_ms() > b.extra_ms()) {
            best = Some(candidate);
        }
    }
    best
}

/// A statement whose runs got much slower at a point in the timeline and stayed slower, and what that establishes.
pub struct Regression<'s> {
    query: &'s QueryStat,
    timeline: &'s Timeline,
    shift: Shift,
    /// When the slow period began: the start of its first bucket.
    since: &'s str,
    /// Hours from then to the timeline's end.
    after_hours: f64,
    /// A slower plan that first ran when it got slower, and the faster one before it.
    plan: Option<PlanChange<'s>>,
    impact: Option<QueryImpact>,
    score: f64,
    severity: Severity,
    confidence: Confidence,
    /// The server started just before the step, with no plan change to explain it: a cold cache may.
    restarted: bool,
}

/// The six statements that got slower by the most, of those whose step is a real part of the workload.
fn regressions(snapshot: &Snapshot) -> Vec<Regression<'_>> {
    let Some(workload) = &snapshot.workload else { return Vec::new() };
    let Some(timeline) = workload.timeline.as_ref().filter(|t| t.len() >= 4) else { return Vec::new() };
    // A bucket of the slow query log averages the runs it kept, the slow ones: a wider tail (one lock episode) reads as
    // the statement getting slower while its typical run didn't change.
    if timeline.source == TimelineSource::SlowLog && crate::queries::logs_slow_runs_only(snapshot) {
        return Vec::new();
    }
    let mut cases: Vec<Regression> = app_queries(snapshot)
        .filter_map(|q| {
            // Only the buckets that account for the statement (`Timeline::statement_bucket`): never one the source
            // can't account for in full, nor one where it ran more than the whole database did.
            let trend = q.trend.as_ref()?;
            let (time_ms, executions) = (0..timeline.len())
                .map(|i| timeline.statement_bucket(trend, i).map_or((None, None), |(t, r)| (Some(t), Some(r))))
                .unzip();
            let s = shift(&QueryTrend { time_ms, executions }, timeline.len())?;
            regression_of(snapshot, timeline, q, s)
        })
        .collect();
    cases.sort_by(|a, b| b.score.total_cmp(&a.score));
    cases.truncate(6);
    cases
}

fn regression_of<'s>(
    snapshot: &Snapshot,
    timeline: &'s Timeline,
    q: &'s QueryStat,
    s: Shift,
) -> Option<Regression<'s>> {
    let (since, _) = timeline.bounds(s.start);
    let since_epoch = parse_epoch_seconds(since)?;
    let end_epoch = parse_epoch_seconds(&timeline.end)?;
    let after_hours = ((end_epoch - since_epoch) / 3600.0).max(1.0 / 60.0);
    let workload_after: f64 = timeline.time_ms[s.start..].iter().sum();
    if workload_after <= 0.0 {
        return None;
    }
    let share_after = (s.after_time_ms / workload_after).clamp(0.0, 1.0);
    let extra_share = (s.extra_ms() / workload_after).clamp(0.0, 1.0);
    if extra_share < 0.01 && share_after < 0.03 {
        return None;
    }
    let plan = plan_change(timeline, q, &s);
    let factor = s.factor();
    // Runs that came in bursts are graded on their total, never extrapolated to a day.
    let amount = severity_for_recent_time(burst_hours(snapshot, q, after_hours), s.after_time_ms, 1.0);
    let mut severity = severity_for_share(share_after).min(amount);
    if factor >= 3.0 && extra_share >= 0.05 {
        severity = severity.max(Severity::Medium.min(amount));
    }
    let mut confidence =
        if timeline.source == TimelineSource::QueryStore { Confidence::High } else { Confidence::Medium };
    // A server that started just before the step runs its first hours on a cold cache: slower, and not for good. Under
    // a day of slow runs since then it is something to watch, not a regression yet.
    let restarted = snapshot
        .server
        .started_at
        .as_deref()
        .and_then(parse_epoch_seconds)
        .filter(|at| (since_epoch - 2.0 * 3600.0..=since_epoch + 3600.0).contains(at))
        .is_some()
        && after_hours < 24.0
        && plan.is_none();
    if restarted {
        severity = Severity::Low;
        confidence = Confidence::Low;
    }
    Some(Regression {
        query: q,
        timeline,
        since,
        after_hours,
        plan,
        impact: query_impact(snapshot, &[q]),
        score: score_for_share(share_after.max(extra_share)),
        severity,
        confidence,
        restarted,
        shift: s,
    })
}

fn regression_finding(snapshot: &Snapshot, regression: Regression<'_>) -> Finding {
    let Regression { query: q, shift: s, since, after_hours, .. } = &regression;
    let (q, since, after_hours) = (*q, *since, *after_hours);
    let when = when_phrase(since);
    let mut f = finding("regression", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    if let Some(impact) = &regression.impact {
        apply_impact(snapshot, &mut f, impact);
    }
    let extra_per_day = s.extra_ms() * 24.0 / after_hours;
    // A day's worth only when the slow period covers most of one; under that it would be an extrapolation.
    f.impact.time_ms_per_day = (after_hours >= culprit_core::PER_DAY_MIN_HOURS).then_some(extra_per_day);
    f.impact.score = regression.score;
    let factor = times(s.factor());
    f.impact.set_label(t!("common.impact.times_slower", factor = factor.clone()));
    f.severity = regression.severity;
    f.confidence = regression.confidence;
    if regression.restarted {
        f.caveats.push(t!(
            "check.regression.caveat.restarted",
            server = snapshot.server.name(),
            when = snapshot.server.started_at.as_deref().map(when_phrase).unwrap_or_default()
        ));
    }
    let query = capitalize(&query_handle(q));
    f.title = match &regression.plan {
        Some(_) => t!("check.regression.title.plan_changed", query = query, factor = factor, when = when.clone()),
        None => t!("check.regression.title", query = query, factor = factor, when = when.clone()),
    };
    let before = format::duration(s.before_ms);
    let mut summary = vec![step_sentence(snapshot, &regression, false)];
    summary.push(match &regression.plan {
        Some(change) => t!(
            "check.regression.summary.plan_change",
            new = &change.new.id,
            first = change.new.first_seen.as_deref().map(when_phrase).unwrap_or_else(|| when.clone()),
            new_ms = format::duration(change.new.mean_time_ms),
            old = &change.old.id,
            old_ms = format::duration(change.old.mean_time_ms)
        ),
        // Several plans, none new around the step: it didn't come with a plan change, which isn't "the plan never changed".
        None if q.workload_plans().len() > 1 => t!("check.regression.summary.no_new_plan"),
        None => t!("check.regression.summary.plan_unknown"),
    });
    f.summary = summary.join(" ");
    f.queries = query_refs(&[q]);
    f.metrics.push(Metric::said(culprit_core::key!("check.regression.metric.before"), s.before_ms, Unit::Ms));
    f.metrics.push(Metric::said(culprit_core::key!("check.regression.metric.since_then"), s.after_ms, Unit::Ms));
    if after_hours >= 20.0 {
        f.metrics.push(Metric::said(culprit_core::key!("common.metric.extra_time_per_day"), extra_per_day, Unit::Ms));
    } else {
        f.metrics.push(Metric::said(culprit_core::key!("common.metric.extra_time"), s.extra_ms(), Unit::Ms));
    }
    f.metrics.push(Metric::said_text(culprit_core::key!("check.regression.metric.started"), short_when(since)));
    if let Some(change) = &regression.plan {
        f.metrics.push(Metric::said_text(
            culprit_core::key!("check.regression.metric.plan_change"),
            t!("check.regression.plan_change", from = &change.old.id, to = &change.new.id),
        ));
        f.facts.plan_change =
            Some(culprit_core::FindingPlanChange { from: change.old.id.clone(), to: change.new.id.clone() });
    }
    f.fix = Some(match &regression.plan {
        Some(change) if snapshot.server.engine == Engine::Mssql && numeric(&q.id) && numeric(&change.old.id) => {
            let mut fix = vec![t!("check.regression.fix.force", plan = &change.old.id)];
            fix.extend(crate::tuning::hand_forcing_note(snapshot));
            Fix {
                summary: fix.join(" "),
                sql: Some(format!(
                    "EXEC sp_query_store_force_plan @query_id = {}, @plan_id = {};",
                    q.id, change.old.id
                )),
                cost: None,
            }
        }
        Some(_) => Fix { summary: t!("check.regression.fix.compare_plans"), sql: None, cost: None },
        None => Fix { summary: t!("check.regression.fix.find_change", when = when), sql: None, cost: None },
    });
    f.verify = vec![t!("check.regression.verify", before = before)];
    if regression.timeline.source == TimelineSource::Scans {
        f.caveats.push(t!("check.regression.caveat.scans"));
    }
    f
}

/// The step in a regression's runs, as its own finding says it ("Until Oct 11, 10:44 UTC it averaged …"), or
/// (`told_elsewhere`) as a finding that takes it in says it ("Query Store's timeline: until …").
fn step_sentence(snapshot: &Snapshot, regression: &Regression<'_>, told_elsewhere: bool) -> String {
    let Regression { shift: s, since, after_hours, .. } = regression;
    let time = time_noun(snapshot);
    let (until, before, after, runs) =
        (short_when(since), format::duration(s.before_ms), format::duration(s.after_ms), format::count(s.after_runs));
    // A day's worth over most of a day of slow runs; under that, their total over the span.
    let (per_day, total) = (format::duration(s.extra_ms() * 24.0 / after_hours), format::duration(s.extra_ms()));
    let span = hours_phrase(*after_hours);
    match (*after_hours >= 20.0, told_elsewhere) {
        (true, false) => t!(
            "check.regression.summary.per_day",
            until = until,
            before = before,
            after = after,
            runs = runs,
            extra = per_day,
            time = time
        ),
        (true, true) => t!(
            "check.regression.timeline.per_day",
            until = until,
            before = before,
            after = after,
            runs = runs,
            extra = per_day,
            time = time
        ),
        (false, false) => t!(
            "check.regression.summary.total",
            until = until,
            before = before,
            after = after,
            runs = runs,
            extra = total,
            time = time,
            span = span
        ),
        (false, true) => t!(
            "check.regression.timeline.total",
            until = until,
            before = before,
            after = after,
            runs = runs,
            extra = total,
            time = time,
            span = span
        ),
    }
}

/// The step in `query`'s runs as a finding that takes its regression in says it, where this scan shows one.
pub(crate) fn regression_timeline(snapshot: &Snapshot, query: &str) -> Option<String> {
    let regression = regressions(snapshot).into_iter().find(|r| r.query.id == query)?;
    Some(step_sentence(snapshot, &regression, true))
}

struct PlanChange<'a> {
    new: &'a PlanVariant,
    old: &'a PlanVariant,
}

/// A plan that first ran around the start of the slow period and is slower than an earlier one.
fn plan_change<'a>(timeline: &Timeline, q: &'a QueryStat, s: &Shift) -> Option<PlanChange<'a>> {
    if q.workload_plans().len() < 2 {
        return None;
    }
    let (start, end) = timeline.bounds(s.start);
    let (start, end) = (parse_epoch_seconds(start)?, parse_epoch_seconds(end)?);
    let previous = s.start.checked_sub(1).and_then(|i| parse_epoch_seconds(timeline.bounds(i).0)).unwrap_or(start);
    let new = q
        .plans
        .iter()
        .filter(|p| p.executions >= 1.0)
        .filter(|p| p.first_seen.as_deref().and_then(parse_epoch_seconds).is_some_and(|t| t >= previous && t <= end))
        .max_by(|a, b| a.executions.total_cmp(&b.executions))?;
    let old = q
        .plans
        .iter()
        .filter(|p| p.id != new.id && p.executions >= 5.0)
        .filter(|p| p.first_seen.as_deref().and_then(parse_epoch_seconds).is_none_or(|t| t < start))
        .min_by(|a, b| a.mean_time_ms.total_cmp(&b.mean_time_ms))?;
    (new.mean_time_ms >= old.mean_time_ms * SLOW_BUCKET).then_some(PlanChange { new, old })
}

/// "3.4×": how many times slower, as people say it (whole from 10×, and "2×" rather than "2.0×").
fn times(factor: f64) -> String {
    let n = if factor >= 10.0 { format!("{}", factor.round()) } else { format!("{factor:.1}") };
    let n = n.strip_suffix(".0").map(str::to_string).unwrap_or(n);
    t!("common.factor", n = format::digits(n))
}

fn numeric(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_digit())
}

/// Each month's short name.
const MONTHS: [&str; 12] = [
    key!("common.month.jan"),
    key!("common.month.feb"),
    key!("common.month.mar"),
    key!("common.month.apr"),
    key!("common.month.may"),
    key!("common.month.jun"),
    key!("common.month.jul"),
    key!("common.month.aug"),
    key!("common.month.sep"),
    key!("common.month.oct"),
    key!("common.month.nov"),
    key!("common.month.dec"),
];

/// The day ("Oct 9") and the time of day in UTC ("14:00") of an ISO 8601 UTC timestamp, if it has both.
fn day_and_time(iso: &str) -> Option<(String, &str)> {
    let month = iso.get(5..7).and_then(|m| m.parse::<usize>().ok()).filter(|m| (1..=12).contains(m))?;
    let day = iso.get(8..10).and_then(|d| d.parse::<u32>().ok())?;
    let time = iso.get(11..16)?;
    let month = culprit_core::i18n::t(MONTHS[month - 1], &[]);
    Some((t!("common.date.month_day", month = month, day = day.to_string()), time))
}

/// "Oct 9, 14:00 UTC" from an ISO 8601 UTC timestamp (anything else as it is).
pub(crate) fn short_when(iso: &str) -> String {
    match day_and_time(iso) {
        Some((day, time)) => t!("common.date.short_utc", day = day, time = time),
        None => iso.to_string(),
    }
}

/// "on Oct 9 at 14:00 UTC": when something happened, for the end of a sentence.
pub(crate) fn when_phrase(iso: &str) -> String {
    match day_and_time(iso) {
        Some((day, time)) => t!("common.date.on_at_utc", day = day, time = time),
        None => t!("common.date.at", when = iso),
    }
}

fn hours_phrase(hours: f64) -> String {
    if hours >= 1.5 {
        t!("check.regression.span.hours", n = hours.round())
    } else {
        t!("check.regression.span.minutes", n = (hours * 60.0).round().max(1.0))
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trend(means: &[f64], runs: f64) -> QueryTrend {
        QueryTrend {
            time_ms: means.iter().map(|m| Some(m * runs)).collect(),
            executions: means.iter().map(|_| Some(runs)).collect(),
        }
    }

    #[test]
    fn finds_where_a_query_stepped_up() {
        let t = trend(&[10.0, 11.0, 9.0, 10.0, 31.0, 29.0, 30.0], 20.0);
        let s = shift(&t, 7).unwrap();
        assert_eq!(s.start, 4);
        assert!((s.before_ms - 10.0).abs() < 1e-9 && (s.after_ms - 30.0).abs() < 1e-9);
    }

    #[test]
    fn ignores_noise_spikes_and_small_changes() {
        assert!(shift(&trend(&[10.0, 10.0, 60.0, 10.0, 10.0, 11.0], 20.0), 6).is_none(), "one spike");
        assert!(shift(&trend(&[10.0, 10.0, 10.0, 15.0, 15.0, 15.0], 20.0), 6).is_none(), "1.5× is not a regression");
        assert!(shift(&trend(&[0.1, 0.1, 0.1, 0.5, 0.5, 0.5], 20.0), 6).is_none(), "fractions of a millisecond");
        assert!(shift(&trend(&[10.0, 10.0, 10.0, 30.0, 30.0], 2.0), 5).is_none(), "too few runs to tell");
    }

    #[test]
    fn unknown_buckets_are_skipped() {
        let mut t = trend(&[10.0, 10.0, 10.0, 30.0, 30.0, 30.0], 20.0);
        t.time_ms[1] = None;
        t.executions[1] = None;
        assert_eq!(shift(&t, 6).unwrap().start, 3);
    }

    #[test]
    fn dates_read_naturally() {
        assert_eq!(short_when("2026-10-09T14:00:00Z"), "Oct 9, 14:00 UTC");
        assert_eq!(when_phrase("2026-10-09T14:00:00Z"), "on Oct 9 at 14:00 UTC");
        assert_eq!((times(3.42), times(2.0), times(12.4)), ("3.4×".into(), "2×".into(), "12×".into()));
    }
}
