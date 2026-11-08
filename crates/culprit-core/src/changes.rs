//! "What changed": two periods of a workload side by side, every query judged with the care Query Store deserves.
//!
//! Averages are weighted by executions, a query needs enough runs in both periods to be compared, and a period the
//! source has no data for is never read as zero. The comparison itself is a pure function of what a source measured,
//! so Query Store intervals and Culprit's own scan history go through the same rules and the same wording.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::format::{self, digits};
use crate::model::{Engine, TimelineSource};
use crate::{key, t};

/// A span of time, UTC ISO 8601, as the source could actually measure it (snapped to Query Store intervals or scans).
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct Period {
    pub start: String,
    pub end: String,
}

impl Period {
    pub fn hours(&self) -> f64 {
        match (epoch(&self.start), epoch(&self.end)) {
            (Some(a), Some(b)) if b > a => (b - a) / 3600.0,
            _ => 0.0,
        }
    }
}

/// What ran in one period: execution-weighted, so the average is the average run, not the average interval.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct PeriodStats {
    pub executions: f64,
    pub total_ms: f64,
    pub mean_ms: f64,
    /// Fastest and slowest single run, where the source keeps them (Query Store does; running totals don't).
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
}

impl PeriodStats {
    pub fn new(executions: f64, total_ms: f64) -> PeriodStats {
        let mean_ms = if executions > 0.0 { total_ms / executions } else { 0.0 };
        PeriodStats { executions, total_ms, mean_ms, min_ms: None, max_ms: None }
    }

    /// Merge two partial aggregates (plans, intervals) of the same period.
    pub fn add(&mut self, other: &PeriodStats) {
        self.executions += other.executions;
        self.total_ms += other.total_ms;
        self.mean_ms = if self.executions > 0.0 { self.total_ms / self.executions } else { 0.0 };
        self.min_ms = min_opt(self.min_ms, other.min_ms);
        self.max_ms = max_opt(self.max_ms, other.max_ms);
    }
}

fn min_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// One plan (or, for sources without plan history, one plan shape Culprit captured) in both periods.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PlanPeriods {
    pub id: String,
    /// Where the plan spends its effort, e.g. "Seq Scan on orders", when known.
    pub label: Option<String>,
    pub baseline: Option<PeriodStats>,
    pub recent: Option<PeriodStats>,
    /// When the plan first ran inside the recent period (UTC ISO 8601).
    pub first_recent: Option<String>,
}

/// What a source measured for one query in each period, before Culprit judges it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct QueryPeriods {
    pub query_id: String,
    pub text: String,
    pub object_name: Option<String>,
    pub baseline: Option<PeriodStats>,
    pub recent: Option<PeriodStats>,
    pub plans: Vec<PlanPeriods>,
    /// Why a missing period can't be read as "didn't run" (sources that only track their heaviest queries). With a
    /// reason, a query seen in one period only is not comparable rather than new or stopped.
    pub untracked: Option<String>,
}

/// Everything a source measured for a comparison.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PeriodData {
    pub source: TimelineSource,
    pub baseline: Period,
    pub recent: Period,
    pub baseline_total: PeriodStats,
    pub recent_total: PeriodStats,
    /// Share of the baseline period the source has data for (0–1). Below one half, a query missing from the baseline
    /// may simply not have been recorded, so it is "not comparable" rather than "new".
    pub baseline_coverage: f64,
    pub queries: Vec<QueryPeriods>,
    pub notes: Vec<String>,
    /// Why the source can't compare these periods at all (too few scans, no data inside them), when it can't.
    #[serde(default)]
    pub unavailable: Option<String>,
}

/// How Culprit judges one query between the periods.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ChangeStatus {
    Regressed,
    New,
    Improved,
    Stopped,
    LowSample,
    NotComparable,
    Unchanged,
}

impl ChangeStatus {
    pub fn label(self) -> String {
        match self {
            ChangeStatus::Regressed => t!("core.changes.status.regressed"),
            ChangeStatus::New => t!("core.changes.status.new"),
            ChangeStatus::Improved => t!("core.changes.status.improved"),
            ChangeStatus::Stopped => t!("core.changes.status.stopped"),
            ChangeStatus::LowSample => t!("core.changes.status.low_sample"),
            ChangeStatus::NotComparable => t!("core.changes.status.not_comparable"),
            ChangeStatus::Unchanged => t!("core.changes.status.unchanged"),
        }
    }
}

/// The plan a query used most changed between the periods.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct PlanSwitch {
    pub from: String,
    pub to: String,
    pub from_label: Option<String>,
    pub to_label: Option<String>,
    /// When the new plan first ran in the recent period.
    pub at: Option<String>,
    /// What differs, in a few words ("Index Seek → Index Scan on customers"), when Culprit compared the two plans.
    pub what: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct QueryChange {
    pub query_id: String,
    pub text: String,
    pub object_name: Option<String>,
    pub status: ChangeStatus,
    pub baseline: Option<PeriodStats>,
    pub recent: Option<PeriodStats>,
    /// Recent average per run over the baseline's, when both periods have enough runs.
    pub ratio: Option<f64>,
    /// Database time the change cost (slower, new) or saved (faster, stopped) over the recent period, in ms.
    pub impact_ms: f64,
    /// Share of the recent period's database time.
    pub recent_share: f64,
    pub plan_switch: Option<PlanSwitch>,
    /// The plan stayed the same in both periods (only said when the source keeps plan history).
    pub same_plan: bool,
    pub headline: String,
    /// Why it couldn't be compared, when it couldn't.
    pub reason: Option<String>,
    /// Changes around then that may explain it (a dropped index on a table it reads, a deploy, throttling), likeliest
    /// first; ids point into `Changes::events`.
    #[serde(default)]
    pub context: Vec<crate::events::EventLink>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Changes {
    pub connection: String,
    pub engine: Engine,
    pub source: TimelineSource,
    pub baseline: Period,
    pub recent: Period,
    pub baseline_total: PeriodStats,
    pub recent_total: PeriodStats,
    pub baseline_coverage: f64,
    pub min_executions: f64,
    /// How much slower or faster (×) counts as a change.
    pub threshold: f64,
    /// One or two sentences about the whole workload.
    pub summary: String,
    /// Changed, new, stopped and not comparable queries, most important first within each status.
    pub queries: Vec<QueryChange>,
    /// Queries that ran about the same in both periods (the heaviest few are listed in `queries`).
    pub unchanged: usize,
    pub notes: Vec<String>,
    /// What changed around the two periods (marks, schema, settings, restarts, throttling), oldest first.
    #[serde(default)]
    pub events: Vec<crate::events::ChangeEvent>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompareOptions {
    /// Runs a query needs in each period before its averages are compared.
    pub min_executions: f64,
    /// Ratio of averages that counts as slower (and its inverse as faster).
    pub threshold: f64,
    /// Smallest change per run that matters, in ms (2× slower from 0.01 ms to 0.02 ms is noise).
    pub min_change_ms: f64,
    /// Unchanged queries kept in the list, heaviest first.
    pub keep_unchanged: usize,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions { min_executions: 10.0, threshold: 1.5, min_change_ms: 1.0, keep_unchanged: 15 }
    }
}

/// Judge every query between the two periods.
pub fn compare(connection: &str, engine: Engine, data: PeriodData, options: &CompareOptions) -> Changes {
    let min = options.min_executions.max(1.0);
    let covered = data.baseline_coverage >= 0.5;
    let recent_total_ms = data.recent_total.total_ms;
    let mut judged: Vec<QueryChange> = data
        .queries
        .into_iter()
        .filter_map(|q| judge(q, engine, data.source, covered, recent_total_ms, min, options))
        .collect();
    judged.sort_by(|a, b| a.status.cmp(&b.status).then(b.impact_ms.total_cmp(&a.impact_ms)));
    let unchanged = judged.iter().filter(|q| q.status == ChangeStatus::Unchanged).count();
    let mut kept_unchanged = 0;
    judged.retain(|q| {
        if q.status != ChangeStatus::Unchanged {
            return true;
        }
        kept_unchanged += 1;
        kept_unchanged <= options.keep_unchanged
    });

    let mut notes = data.notes;
    // A comparison that couldn't be made has no coverage to describe: its reason leads the summary instead.
    if data.unavailable.is_none() && data.baseline_coverage < 0.999 && data.baseline.hours() > 0.0 {
        let share = format::percent(data.baseline_coverage);
        notes.push(match data.source {
            TimelineSource::QueryStore => t!("core.changes.coverage.query_store", share = share),
            TimelineSource::Scans => t!("core.changes.coverage.scans", share = share),
            TimelineSource::SlowLog => t!("core.changes.coverage.slow_log", share = share),
        });
    }
    // A comparison that couldn't be made says so first, rather than "no queries ran".
    let summary = match &data.unavailable {
        Some(why) => [t!("core.changes.unavailable"), why.clone()].join(" "),
        None => summarize(&judged, &data.baseline, &data.recent, &data.baseline_total, &data.recent_total),
    };
    Changes {
        connection: connection.to_string(),
        engine,
        source: data.source,
        baseline: data.baseline,
        recent: data.recent,
        baseline_total: data.baseline_total,
        recent_total: data.recent_total,
        baseline_coverage: data.baseline_coverage,
        min_executions: min,
        threshold: options.threshold,
        summary,
        queries: judged,
        unchanged,
        notes,
        events: Vec::new(),
    }
}

fn judge(
    q: QueryPeriods,
    engine: Engine,
    source: TimelineSource,
    covered: bool,
    recent_total_ms: f64,
    min: f64,
    options: &CompareOptions,
) -> Option<QueryChange> {
    let b = q.baseline.clone().filter(|s| s.executions > 0.0);
    let r = q.recent.clone().filter(|s| s.executions > 0.0);
    let recent_share = r.as_ref().map(|r| share(r.total_ms, recent_total_ms)).unwrap_or(0.0);
    let one_sided = b.is_none() != r.is_none();
    // A handful of runs seen in one period only (a one-off script, a migration) is noise unless it was heavy.
    let side_runs = b.as_ref().or(r.as_ref()).map(|s| s.executions).unwrap_or(0.0);
    if one_sided && side_runs < min && recent_share < 0.01 {
        return None;
    }
    let (status, ratio, impact_ms) = match (&b, &r) {
        (None, None) => return None,
        (None, Some(r)) if one_sided && q.untracked.is_some() => (ChangeStatus::NotComparable, None, r.total_ms),
        (Some(b), None) if one_sided && q.untracked.is_some() && b.executions >= min => {
            (ChangeStatus::NotComparable, None, b.total_ms)
        }
        (None, Some(r)) if !covered => (ChangeStatus::NotComparable, None, r.total_ms),
        (None, Some(r)) if r.executions >= min => (ChangeStatus::New, None, r.total_ms),
        (None, Some(r)) => (ChangeStatus::LowSample, None, r.total_ms),
        (Some(b), None) if b.executions >= min => (ChangeStatus::Stopped, None, b.total_ms),
        // A handful of runs that didn't come back is noise, not news.
        (Some(_), None) => return None,
        (Some(b), Some(r)) if b.executions < min || r.executions < min => (ChangeStatus::LowSample, None, r.total_ms),
        (Some(b), Some(r)) => {
            let ratio = if b.mean_ms > 0.0 { r.mean_ms / b.mean_ms } else { f64::INFINITY };
            let diff = r.mean_ms - b.mean_ms;
            if ratio >= options.threshold && diff >= options.min_change_ms {
                (ChangeStatus::Regressed, Some(ratio), diff * r.executions)
            } else if ratio <= 1.0 / options.threshold && -diff >= options.min_change_ms {
                (ChangeStatus::Improved, Some(ratio), -diff * r.executions)
            } else {
                (ChangeStatus::Unchanged, Some(ratio), r.total_ms)
            }
        }
    };
    let (plan_switch, same_plan) = plan_switch(&q.plans);
    let reason = match status {
        ChangeStatus::NotComparable => Some(q.untracked.clone().unwrap_or_else(|| match source {
            TimelineSource::QueryStore => t!("core.changes.reason.query_store"),
            TimelineSource::Scans => t!("core.changes.reason.scans"),
            TimelineSource::SlowLog => t!("core.changes.reason.slow_log"),
        })),
        _ => None,
    };
    let mut change = QueryChange {
        query_id: q.query_id,
        text: q.text,
        object_name: q.object_name,
        status,
        baseline: b,
        recent: r,
        ratio: ratio.filter(|r| r.is_finite()),
        impact_ms,
        recent_share,
        plan_switch,
        same_plan,
        headline: String::new(),
        reason,
        context: Vec::new(),
    };
    change.headline = headline(&change, engine, source, min);
    Some(change)
}

/// The plan each period used most, and whether that changed. Plans need runs in both periods to say "same plan".
fn plan_switch(plans: &[PlanPeriods]) -> (Option<PlanSwitch>, bool) {
    let busiest = |pick: fn(&PlanPeriods) -> Option<&PeriodStats>| {
        plans
            .iter()
            .filter_map(|p| Some((p, pick(p)?.executions)))
            .filter(|(_, runs)| *runs > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(p, _)| p)
    };
    let (Some(before), Some(after)) = (busiest(|p| p.baseline.as_ref()), busiest(|p| p.recent.as_ref())) else {
        return (None, false);
    };
    if before.id == after.id {
        return (None, true);
    }
    let switch = PlanSwitch {
        from: before.id.clone(),
        to: after.id.clone(),
        from_label: before.label.clone(),
        to_label: after.label.clone(),
        at: after.first_recent.clone(),
        what: match (&before.label, &after.label) {
            (Some(a), Some(b)) if a != b => Some(format!("{a} → {b}")),
            _ => None,
        },
    };
    (Some(switch), false)
}

fn share(part: f64, whole: f64) -> f64 {
    if whole > 0.0 { (part / whole).clamp(0.0, 1.0) } else { 0.0 }
}

/// "4.1×" for a ratio.
pub fn times(ratio: f64) -> String {
    let n = if ratio >= 10.0 { format!("{ratio:.0}") } else { format!("{ratio:.1}") };
    t!("common.factor", n = digits(n))
}

/// Plain-language sentence for one query. Times are UTC here; the app shows them in local time.
pub fn headline(c: &QueryChange, engine: Engine, source: TimelineSource, min: f64) -> String {
    use crate::format::{count, percent};
    let per_run = |b: &PeriodStats, r: &PeriodStats| {
        t!("core.changes.per_run", before = run_time(b.mean_ms), after = run_time(r.mean_ms))
    };
    // What switched: SQL Server's Query Store numbers its plans; elsewhere it is the plan Culprit captured.
    let switch_words = |s: &PlanSwitch| -> String {
        match ((engine, source), s.what.as_deref()) {
            ((Engine::Mssql, TimelineSource::QueryStore), None) => {
                t!("core.changes.switch.query_store", from = &s.from, to = &s.to)
            }
            ((Engine::Mssql, TimelineSource::QueryStore), Some(what)) => {
                t!("core.changes.switch.query_store_what", from = &s.from, to = &s.to, what = what)
            }
            (_, None) => t!("core.changes.switch.captured"),
            (_, Some(what)) => t!("core.changes.switch.captured_what", what = what),
        }
    };
    match (c.status, &c.baseline, &c.recent) {
        (ChangeStatus::Regressed, Some(b), Some(r)) => {
            let ratio = c.ratio.map(times).unwrap_or_else(|| t!("core.changes.much"));
            match &c.plan_switch {
                Some(s) => match s.at.as_deref() {
                    Some(at) => t!(
                        "core.changes.headline.slower_switch_at",
                        ratio = ratio,
                        per_run = per_run(b, r),
                        at = short_time(at),
                        switch = switch_words(s)
                    ),
                    None => t!(
                        "core.changes.headline.slower_switch",
                        ratio = ratio,
                        per_run = per_run(b, r),
                        switch = switch_words(s)
                    ),
                },
                None if c.same_plan && source == TimelineSource::Scans => {
                    t!("core.changes.headline.slower_same_estimated_plan", ratio = ratio, per_run = per_run(b, r))
                }
                None if c.same_plan => {
                    t!("core.changes.headline.slower_same_plan", ratio = ratio, per_run = per_run(b, r))
                }
                None => t!("core.changes.headline.slower", ratio = ratio, per_run = per_run(b, r)),
            }
        }
        (ChangeStatus::Improved, Some(b), Some(r)) => {
            let ratio = c.ratio.map(|x| times(1.0 / x)).unwrap_or_default();
            match &c.plan_switch {
                Some(s) => match s.at.as_deref() {
                    Some(at) => t!(
                        "core.changes.headline.faster_switch_at",
                        ratio = ratio,
                        per_run = per_run(b, r),
                        at = short_time(at),
                        switch = switch_words(s)
                    ),
                    None => t!(
                        "core.changes.headline.faster_switch",
                        ratio = ratio,
                        per_run = per_run(b, r),
                        switch = switch_words(s)
                    ),
                },
                None => t!("core.changes.headline.faster", ratio = ratio, per_run = per_run(b, r)),
            }
        }
        (ChangeStatus::New, _, Some(r)) => t!(
            "core.changes.headline.new",
            n = r.executions,
            count = count(r.executions),
            mean = run_time(r.mean_ms),
            share = percent(c.recent_share)
        ),
        (ChangeStatus::Stopped, Some(b), _) => {
            t!("core.changes.headline.stopped", times = times_word(b.executions), mean = run_time(b.mean_ms))
        }
        (ChangeStatus::LowSample, b, r) => t!(
            "core.changes.headline.low_sample",
            before = count(b.as_ref().map(|s| s.executions).unwrap_or(0.0)),
            after = count(r.as_ref().map(|s| s.executions).unwrap_or(0.0)),
            min = count(min)
        ),
        (ChangeStatus::NotComparable, b, r) => {
            let why = c.reason.clone().unwrap_or_else(|| t!("core.changes.reason.no_data"));
            match (b, r) {
                (Some(b), None) => {
                    t!("core.changes.headline.not_comparable_before", times = times_word(b.executions), why = why)
                }
                (_, r) => t!(
                    "core.changes.headline.not_comparable_recent",
                    times = times_word(r.as_ref().map(|s| s.executions).unwrap_or(0.0)),
                    why = why
                ),
            }
        }
        (_, Some(b), Some(r)) => t!("core.changes.headline.unchanged", per_run = per_run(b, r)),
        _ => c.status.label(),
    }
}

fn summarize(
    queries: &[QueryChange],
    baseline: &Period,
    recent: &Period,
    baseline_total: &PeriodStats,
    recent_total: &PeriodStats,
) -> String {
    use crate::format::duration;
    let per_hour = |s: &PeriodStats, p: &Period| if p.hours() > 0.0 { s.total_ms / p.hours() } else { 0.0 };
    let (before, after) = (per_hour(baseline_total, baseline), per_hour(recent_total, recent));
    // Rates per hour are only honest when the periods are at least that long; otherwise say what was measured.
    let short = baseline.hours() < 1.0 || recent.hours() < 1.0;
    let count = |status: ChangeStatus| queries.iter().filter(|q| q.status == status).count();
    let (slower, new, faster) =
        (count(ChangeStatus::Regressed), count(ChangeStatus::New), count(ChangeStatus::Improved));
    let workload = if before > 0.0 && after > 0.0 {
        let ratio = after / before;
        let (rose, fell) = (ratio >= 1.15, ratio <= 1.0 / 1.15);
        if short {
            let (before, before_span) = (duration(baseline_total.total_ms), span_words(baseline.hours()));
            let (after, after_span) = (duration(recent_total.total_ms), span_words(recent.hours()));
            if rose {
                t!(
                    "core.changes.workload.covered_rose",
                    factor = times(ratio),
                    before = before,
                    before_span = before_span,
                    after = after,
                    after_span = after_span
                )
            } else if fell {
                t!(
                    "core.changes.workload.covered_fell",
                    share = format::percent(ratio),
                    before = before,
                    before_span = before_span,
                    after = after,
                    after_span = after_span
                )
            } else {
                t!(
                    "core.changes.workload.covered_same",
                    before = before,
                    before_span = before_span,
                    after = after,
                    after_span = after_span
                )
            }
        } else if rose {
            t!(
                "core.changes.workload.per_hour_rose",
                factor = times(ratio),
                before = duration(before),
                after = duration(after)
            )
        } else if fell {
            t!(
                "core.changes.workload.per_hour_fell",
                share = format::percent(ratio),
                before = duration(before),
                after = duration(after)
            )
        } else {
            t!("core.changes.workload.per_hour_same", before = duration(before), after = duration(after))
        }
    } else if after > 0.0 {
        t!("core.changes.workload.recent_only", after = duration(after))
    } else {
        t!("core.changes.workload.none")
    };
    let parts: Vec<String> = [
        (slower, key!("core.changes.counts.slower")),
        (new, key!("core.changes.counts.new")),
        (faster, key!("core.changes.counts.faster")),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, key)| crate::i18n::t(key, &[("n", n.into()), ("count", n.to_string().into())]))
    .collect();
    let changed = if parts.is_empty() {
        t!("core.changes.counts.none")
    } else {
        t!("core.changes.counts.changed", changes = format::list(&parts))
    };
    [workload, changed].join(" ")
}

/// "3 s", "12 min", "5 h".
fn span_words(hours: f64) -> String {
    format::duration(hours * 3_600_000.0)
}

/// A single run's duration with enough precision to compare two of them ("8.2 ms", not "8 ms").
pub fn run_time(ms: f64) -> String {
    if (1.0..10.0).contains(&ms) {
        t!("common.duration.ms", n = digits(format!("{ms:.1}")))
    } else {
        format::duration(ms)
    }
}

/// "once", "twice", "1,240 times".
fn times_word(n: f64) -> String {
    t!("core.changes.times", n = n.round(), count = format::count(n))
}

/// "Tue 14:05 UTC".
pub fn short_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|t| {
            let t = t.with_timezone(&chrono::Utc);
            t!("core.changes.time.short", weekday = weekday(&t), time = t.format("%H:%M").to_string())
        })
        .unwrap_or_else(|_| iso.to_string())
}

/// "Sun Oct 4 18:00 UTC": [`short_time`] with the date, for ranges longer than a day.
pub fn dated_time(iso: &str) -> String {
    use chrono::Datelike;
    // Each month's short name.
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
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|t| {
            let t = t.with_timezone(&chrono::Utc);
            t!(
                "core.changes.time.dated",
                weekday = weekday(&t),
                month = crate::i18n::t(MONTHS[t.month0() as usize], &[]),
                day = t.day().to_string(),
                time = t.format("%H:%M").to_string()
            )
        })
        .unwrap_or_else(|_| iso.to_string())
}

/// A day of the week's short name: "Tue".
fn weekday(t: &chrono::DateTime<chrono::Utc>) -> String {
    use chrono::{Datelike, Weekday};
    match t.weekday() {
        Weekday::Mon => t!("common.weekday.mon"),
        Weekday::Tue => t!("common.weekday.tue"),
        Weekday::Wed => t!("common.weekday.wed"),
        Weekday::Thu => t!("common.weekday.thu"),
        Weekday::Fri => t!("common.weekday.fri"),
        Weekday::Sat => t!("common.weekday.sat"),
        Weekday::Sun => t!("common.weekday.sun"),
    }
}

/// Both ends of a range: weekday and time within a day, with dates once it spans more than one.
pub fn range_times(start: &str, end: &str) -> (String, String) {
    let long = match (epoch(start), epoch(end)) {
        (Some(a), Some(b)) => (b - a).abs() > 20.0 * 3600.0,
        _ => false,
    };
    if long { (dated_time(start), dated_time(end)) } else { (short_time(start), short_time(end)) }
}

/// `2026-10-11 14:05:00` (UTC) for an ISO 8601 time, to embed in SQL. `None` for anything else, so only a time Culprit
/// formatted itself reaches a statement.
pub fn sql_timestamp(iso: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S").to_string())
}

pub fn epoch(text: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(text).ok().map(|t| t.timestamp_millis() as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_longer_than_a_day_carry_dates() {
        let (a, b) = range_times("2026-10-04T18:00:00Z", "2026-10-11T18:00:00Z");
        assert_eq!((a.as_str(), b.as_str()), ("Sun Oct 4 18:00 UTC", "Sun Oct 11 18:00 UTC"));
        let (a, b) = range_times("2026-10-11T12:00:00Z", "2026-10-11T18:00:00Z");
        assert_eq!((a.as_str(), b.as_str()), ("Sun 12:00 UTC", "Sun 18:00 UTC"));
    }

    fn stats(runs: f64, mean: f64) -> PeriodStats {
        PeriodStats::new(runs, runs * mean)
    }

    fn data(queries: Vec<QueryPeriods>, coverage: f64) -> PeriodData {
        let mut baseline_total = PeriodStats::default();
        let mut recent_total = PeriodStats::default();
        for q in &queries {
            if let Some(b) = &q.baseline {
                baseline_total.add(b);
            }
            if let Some(r) = &q.recent {
                recent_total.add(r);
            }
        }
        PeriodData {
            source: TimelineSource::QueryStore,
            baseline: Period { start: "2026-10-11T06:00:00Z".into(), end: "2026-10-11T10:00:00Z".into() },
            recent: Period { start: "2026-10-11T10:00:00Z".into(), end: "2026-10-11T14:00:00Z".into() },
            baseline_total,
            recent_total,
            baseline_coverage: coverage,
            queries,
            notes: vec![],
            unavailable: None,
        }
    }

    fn query(id: &str, b: Option<PeriodStats>, r: Option<PeriodStats>) -> QueryPeriods {
        QueryPeriods { query_id: id.into(), text: "SELECT 1".into(), baseline: b, recent: r, ..Default::default() }
    }

    #[test]
    fn statuses_follow_the_rules() {
        let changes = compare(
            "shop",
            Engine::Mssql,
            data(
                vec![
                    query("slower", Some(stats(100.0, 2.0)), Some(stats(120.0, 8.2))),
                    query("faster", Some(stats(100.0, 40.0)), Some(stats(100.0, 16.0))),
                    query("same", Some(stats(100.0, 5.0)), Some(stats(100.0, 5.5))),
                    query("new", None, Some(stats(50.0, 3.0))),
                    query("gone", Some(stats(50.0, 3.0)), None),
                    query("few", Some(stats(3.0, 3.0)), Some(stats(500.0, 30.0))),
                    query("tiny", Some(stats(100.0, 0.01)), Some(stats(100.0, 0.05))),
                    query("blip", Some(stats(2.0, 3.0)), None),
                ],
                1.0,
            ),
            &CompareOptions::default(),
        );
        let status = |id: &str| changes.queries.iter().find(|q| q.query_id == id).map(|q| q.status);
        assert_eq!(status("slower"), Some(ChangeStatus::Regressed));
        assert_eq!(status("faster"), Some(ChangeStatus::Improved));
        assert_eq!(status("same"), Some(ChangeStatus::Unchanged));
        assert_eq!(status("new"), Some(ChangeStatus::New));
        assert_eq!(status("gone"), Some(ChangeStatus::Stopped));
        assert_eq!(status("few"), Some(ChangeStatus::LowSample), "3 runs before is too few");
        assert_eq!(status("tiny"), Some(ChangeStatus::Unchanged), "5× of 0.01 ms is noise");
        assert_eq!(status("blip"), None);
        assert_eq!(changes.queries[0].query_id, "slower", "regressions come first");
        assert_eq!(changes.unchanged, 2);
        assert_eq!(changes.queries[0].headline, "Got 4.1× slower (2.0 ms → 8.2 ms per run).");
        assert!(changes.summary.contains("1 query got slower"), "{}", changes.summary);
    }

    #[test]
    fn a_missing_baseline_is_never_zero() {
        let only_recent = vec![query("new", None, Some(stats(50.0, 3.0)))];
        let changes = compare("shop", Engine::Mssql, data(only_recent, 0.2), &CompareOptions::default());
        assert_eq!(changes.queries[0].status, ChangeStatus::NotComparable);
        assert!(changes.notes[0].contains("20%"), "{:?}", changes.notes);
    }

    #[test]
    fn untracked_queries_are_not_comparable() {
        let mut q = query("seen-late", None, Some(stats(80.0, 3.0)));
        q.untracked = Some("Culprit's earlier scans didn't track it".into());
        let changes = compare("shop", Engine::Postgres, data(vec![q], 1.0), &CompareOptions::default());
        assert_eq!(changes.queries[0].status, ChangeStatus::NotComparable);
        assert!(changes.queries[0].headline.contains("didn't track it"), "{}", changes.queries[0].headline);
    }

    #[test]
    fn short_periods_report_what_was_measured() {
        let mut d = data(vec![query("q", Some(stats(100.0, 1.0)), Some(stats(100.0, 3.0)))], 1.0);
        d.baseline = Period { start: "2026-10-11T10:00:00Z".into(), end: "2026-10-11T10:00:02Z".into() };
        d.recent = Period { start: "2026-10-11T10:00:02Z".into(), end: "2026-10-11T10:00:05Z".into() };
        let changes = compare("shop", Engine::Postgres, d, &CompareOptions::default());
        assert!(changes.summary.contains("for the time covered"), "{}", changes.summary);
        assert!(!changes.summary.contains("per hour"), "{}", changes.summary);
    }

    // What a comparison says about every kind of query and the whole workload, word for word: from locales/en.json.
    #[test]
    fn changes_say_what_happened_in_english() {
        let mut out = Vec::new();
        for status in [
            ChangeStatus::Regressed,
            ChangeStatus::New,
            ChangeStatus::Improved,
            ChangeStatus::Stopped,
            ChangeStatus::LowSample,
            ChangeStatus::NotComparable,
            ChangeStatus::Unchanged,
        ] {
            out.push(format!("{status:?}: {}", status.label()));
        }
        let change = |status, b: Option<PeriodStats>, r: Option<PeriodStats>, ratio: Option<f64>| QueryChange {
            query_id: "q".into(),
            text: "SELECT 1".into(),
            object_name: None,
            status,
            baseline: b,
            recent: r,
            ratio,
            impact_ms: 0.0,
            recent_share: 0.123,
            plan_switch: None,
            same_plan: false,
            headline: String::new(),
            reason: None,
            context: vec![],
        };
        let switch = |at: Option<&str>, what: Option<&str>| PlanSwitch {
            from: "815".into(),
            to: "685".into(),
            from_label: None,
            to_label: None,
            at: at.map(str::to_string),
            what: what.map(str::to_string),
        };
        let (b, r) = (Some(stats(100.0, 2.0)), Some(stats(1500.0, 0.4)));
        let mut cases = vec![
            change(ChangeStatus::Regressed, b.clone(), r.clone(), Some(4.13)),
            change(ChangeStatus::Regressed, b.clone(), r.clone(), Some(41.3)),
            change(ChangeStatus::Regressed, b.clone(), r.clone(), None),
            QueryChange { same_plan: true, ..change(ChangeStatus::Regressed, b.clone(), r.clone(), Some(2.0)) },
            change(ChangeStatus::Improved, b.clone(), r.clone(), Some(0.2)),
            change(ChangeStatus::Improved, b.clone(), r.clone(), None),
            change(ChangeStatus::New, None, r.clone(), None),
            change(ChangeStatus::Stopped, Some(stats(1.0, 2.0)), None, None),
            change(ChangeStatus::Stopped, Some(stats(2.0, 2.0)), None, None),
            change(ChangeStatus::Stopped, Some(stats(1240.0, 0.004)), None, None),
            change(ChangeStatus::LowSample, Some(stats(3.0, 2.0)), r.clone(), None),
            change(ChangeStatus::LowSample, None, None, None),
            change(ChangeStatus::NotComparable, Some(stats(30.0, 2.0)), None, None),
            change(ChangeStatus::NotComparable, None, r.clone(), None),
            QueryChange {
                reason: Some("Query Store has no data for the baseline period".into()),
                ..change(ChangeStatus::NotComparable, None, Some(stats(1.0, 2.0)), None)
            },
            change(ChangeStatus::Unchanged, b.clone(), Some(stats(100.0, 2_500.0)), Some(1.0)),
            change(ChangeStatus::Unchanged, None, None, None),
        ];
        for (at, what) in [(Some("2026-10-13T14:05:00Z"), Some("Index Seek → Index Scan")), (None, None)] {
            for status in [ChangeStatus::Regressed, ChangeStatus::Improved] {
                let ratio = if status == ChangeStatus::Regressed { 3.0 } else { 0.5 };
                cases.push(QueryChange {
                    plan_switch: Some(switch(at, what)),
                    ..change(status, b.clone(), r.clone(), Some(ratio))
                });
            }
        }
        for (engine, source) in [
            (Engine::Mssql, TimelineSource::QueryStore),
            (Engine::Postgres, TimelineSource::Scans),
            (Engine::Mysql, TimelineSource::SlowLog),
        ] {
            for c in &cases {
                out.push(format!("{engine:?} {source:?} {:?}: {}", c.status, headline(c, engine, source, 10.0)));
            }
        }
        // The whole workload, with each source's coverage and each trend.
        for source in [TimelineSource::QueryStore, TimelineSource::Scans, TimelineSource::SlowLog] {
            let mut d = data(vec![query("q", Some(stats(100.0, 1.0)), Some(stats(100.0, 3.0)))], 0.4);
            d.source = source;
            let c = compare("shop", Engine::Postgres, d, &CompareOptions::default());
            out.push(format!("{source:?}: {} | {}", c.summary, c.notes.join(" | ")));
        }
        let mut d = data(vec![], 1.0);
        d.unavailable = Some("Culprit has one scan in the baseline period; it needs two.".into());
        out.push(compare("shop", Engine::Postgres, d, &CompareOptions::default()).summary);
        out.push(compare("shop", Engine::Postgres, data(vec![], 1.0), &CompareOptions::default()).summary);
        let only_recent = vec![query("n", None, Some(stats(100.0, 3.0)))];
        out.push(compare("shop", Engine::Postgres, data(only_recent, 1.0), &CompareOptions::default()).summary);
        let mut queries = vec![
            query("s1", Some(stats(100.0, 2.0)), Some(stats(100.0, 8.0))),
            query("s2", Some(stats(100.0, 2.0)), Some(stats(100.0, 9.0))),
            query("f1", Some(stats(100.0, 40.0)), Some(stats(100.0, 10.0))),
            query("n1", None, Some(stats(100.0, 3.0))),
        ];
        out.push(compare("shop", Engine::Postgres, data(queries.clone(), 1.0), &CompareOptions::default()).summary);
        queries.push(query("n2", None, Some(stats(100.0, 3.0))));
        queries.remove(1);
        out.push(compare("shop", Engine::Postgres, data(queries.clone(), 1.0), &CompareOptions::default()).summary);
        let falls = vec![query("f", Some(stats(100.0, 40.0)), Some(stats(100.0, 4.0)))];
        out.push(compare("shop", Engine::Postgres, data(falls, 1.0), &CompareOptions::default()).summary);
        let steady = vec![query("u", Some(stats(100.0, 4.0)), Some(stats(100.0, 4.2)))];
        out.push(compare("shop", Engine::Postgres, data(steady, 1.0), &CompareOptions::default()).summary);
        let mut short = data(vec![query("q", Some(stats(100.0, 1.0)), Some(stats(100.0, 3.0)))], 1.0);
        short.baseline = Period { start: "2026-10-11T10:00:00Z".into(), end: "2026-10-11T10:00:02Z".into() };
        short.recent = Period { start: "2026-10-11T10:00:02Z".into(), end: "2026-10-11T10:40:05Z".into() };
        out.push(compare("shop", Engine::Postgres, short, &CompareOptions::default()).summary);
        for ms in [0.004, 0.5, 5.0, 50.0, 5_000.0] {
            out.push(format!("{ms}: {}", run_time(ms)));
        }
        for iso in ["2026-10-13T14:05:00Z", "2026-01-04T08:00:00+02:00", "not a time"] {
            out.push(format!("{iso}: {} | {}", short_time(iso), dated_time(iso)));
        }
        out.push(format!("{} {} {}", times(4.13), times(41.3), times(1.0)));
        let got = out.join("\n");
        assert_eq!(
            got,
            r#"Regressed: Got slower
New: New
Improved: Got faster
Stopped: Stopped running
LowSample: Too few runs
NotComparable: Not comparable
Unchanged: About the same
Mssql QueryStore Regressed: Got 4.1× slower (2.0 ms → 0.40 ms per run).
Mssql QueryStore Regressed: Got 41× slower (2.0 ms → 0.40 ms per run).
Mssql QueryStore Regressed: Got much slower (2.0 ms → 0.40 ms per run).
Mssql QueryStore Regressed: Got 2.0× slower (2.0 ms → 0.40 ms per run) with the same plan: more data, blocking or a busier server.
Mssql QueryStore Improved: Got 5.0× faster (2.0 ms → 0.40 ms per run).
Mssql QueryStore Improved: Got  faster (2.0 ms → 0.40 ms per run).
Mssql QueryStore New: Started running: 1.5K runs averaging 0.40 ms, 12% of recent database time.
Mssql QueryStore Stopped: Stopped running: it ran once before, averaging 2.0 ms.
Mssql QueryStore Stopped: Stopped running: it ran twice before, averaging 2.0 ms.
Mssql QueryStore Stopped: Stopped running: it ran 1.2K times before, averaging 4 µs.
Mssql QueryStore LowSample: Too few runs to compare: 3 before and 1.5K after (Culprit needs 10 in each).
Mssql QueryStore LowSample: Too few runs to compare: 0 before and 0 after (Culprit needs 10 in each).
Mssql QueryStore NotComparable: Ran 30 times before; can't tell whether it still runs: there is no data for the other period.
Mssql QueryStore NotComparable: Ran 1.5K times recently, but can't be compared: there is no data for the other period.
Mssql QueryStore NotComparable: Ran once recently, but can't be compared: Query Store has no data for the baseline period.
Mssql QueryStore Unchanged: About the same (2.0 ms → 2.5 s per run).
Mssql QueryStore Unchanged: About the same
Mssql QueryStore Regressed: Got 3.0× slower (2.0 ms → 0.40 ms per run) on Tue 14:05 UTC, when SQL Server switched from plan 815 to plan 685 (Index Seek → Index Scan).
Mssql QueryStore Improved: Got 2.0× faster (2.0 ms → 0.40 ms per run) on Tue 14:05 UTC, when SQL Server switched from plan 815 to plan 685 (Index Seek → Index Scan).
Mssql QueryStore Regressed: Got 3.0× slower (2.0 ms → 0.40 ms per run), when SQL Server switched from plan 815 to plan 685.
Mssql QueryStore Improved: Got 2.0× faster (2.0 ms → 0.40 ms per run), when SQL Server switched from plan 815 to plan 685.
Postgres Scans Regressed: Got 4.1× slower (2.0 ms → 0.40 ms per run).
Postgres Scans Regressed: Got 41× slower (2.0 ms → 0.40 ms per run).
Postgres Scans Regressed: Got much slower (2.0 ms → 0.40 ms per run).
Postgres Scans Regressed: Got 2.0× slower (2.0 ms → 0.40 ms per run) with the same estimated plan: more data, blocking or a busier server.
Postgres Scans Improved: Got 5.0× faster (2.0 ms → 0.40 ms per run).
Postgres Scans Improved: Got  faster (2.0 ms → 0.40 ms per run).
Postgres Scans New: Started running: 1.5K runs averaging 0.40 ms, 12% of recent database time.
Postgres Scans Stopped: Stopped running: it ran once before, averaging 2.0 ms.
Postgres Scans Stopped: Stopped running: it ran twice before, averaging 2.0 ms.
Postgres Scans Stopped: Stopped running: it ran 1.2K times before, averaging 4 µs.
Postgres Scans LowSample: Too few runs to compare: 3 before and 1.5K after (Culprit needs 10 in each).
Postgres Scans LowSample: Too few runs to compare: 0 before and 0 after (Culprit needs 10 in each).
Postgres Scans NotComparable: Ran 30 times before; can't tell whether it still runs: there is no data for the other period.
Postgres Scans NotComparable: Ran 1.5K times recently, but can't be compared: there is no data for the other period.
Postgres Scans NotComparable: Ran once recently, but can't be compared: Query Store has no data for the baseline period.
Postgres Scans Unchanged: About the same (2.0 ms → 2.5 s per run).
Postgres Scans Unchanged: About the same
Postgres Scans Regressed: Got 3.0× slower (2.0 ms → 0.40 ms per run) on Tue 14:05 UTC, when the plan Culprit captured for it changed (Index Seek → Index Scan).
Postgres Scans Improved: Got 2.0× faster (2.0 ms → 0.40 ms per run) on Tue 14:05 UTC, when the plan Culprit captured for it changed (Index Seek → Index Scan).
Postgres Scans Regressed: Got 3.0× slower (2.0 ms → 0.40 ms per run), when the plan Culprit captured for it changed.
Postgres Scans Improved: Got 2.0× faster (2.0 ms → 0.40 ms per run), when the plan Culprit captured for it changed.
Mysql SlowLog Regressed: Got 4.1× slower (2.0 ms → 0.40 ms per run).
Mysql SlowLog Regressed: Got 41× slower (2.0 ms → 0.40 ms per run).
Mysql SlowLog Regressed: Got much slower (2.0 ms → 0.40 ms per run).
Mysql SlowLog Regressed: Got 2.0× slower (2.0 ms → 0.40 ms per run) with the same plan: more data, blocking or a busier server.
Mysql SlowLog Improved: Got 5.0× faster (2.0 ms → 0.40 ms per run).
Mysql SlowLog Improved: Got  faster (2.0 ms → 0.40 ms per run).
Mysql SlowLog New: Started running: 1.5K runs averaging 0.40 ms, 12% of recent database time.
Mysql SlowLog Stopped: Stopped running: it ran once before, averaging 2.0 ms.
Mysql SlowLog Stopped: Stopped running: it ran twice before, averaging 2.0 ms.
Mysql SlowLog Stopped: Stopped running: it ran 1.2K times before, averaging 4 µs.
Mysql SlowLog LowSample: Too few runs to compare: 3 before and 1.5K after (Culprit needs 10 in each).
Mysql SlowLog LowSample: Too few runs to compare: 0 before and 0 after (Culprit needs 10 in each).
Mysql SlowLog NotComparable: Ran 30 times before; can't tell whether it still runs: there is no data for the other period.
Mysql SlowLog NotComparable: Ran 1.5K times recently, but can't be compared: there is no data for the other period.
Mysql SlowLog NotComparable: Ran once recently, but can't be compared: Query Store has no data for the baseline period.
Mysql SlowLog Unchanged: About the same (2.0 ms → 2.5 s per run).
Mysql SlowLog Unchanged: About the same
Mysql SlowLog Regressed: Got 3.0× slower (2.0 ms → 0.40 ms per run) on Tue 14:05 UTC, when the plan Culprit captured for it changed (Index Seek → Index Scan).
Mysql SlowLog Improved: Got 2.0× faster (2.0 ms → 0.40 ms per run) on Tue 14:05 UTC, when the plan Culprit captured for it changed (Index Seek → Index Scan).
Mysql SlowLog Regressed: Got 3.0× slower (2.0 ms → 0.40 ms per run), when the plan Culprit captured for it changed.
Mysql SlowLog Improved: Got 2.0× faster (2.0 ms → 0.40 ms per run), when the plan Culprit captured for it changed.
QueryStore: Database time per hour rose 3.0×: 25 ms → 75 ms. 1 query got slower. | Query Store has data for 40% of the baseline period; queries missing from it may not have been recorded.
Scans: Database time per hour rose 3.0×: 25 ms → 75 ms. 1 query got slower. | Culprit's scans cover 40% of the baseline period; it follows the scans it has.
SlowLog: Database time per hour rose 3.0×: 25 ms → 75 ms. 1 query got slower. | The slow query log covers 40% of the baseline period; statements missing from it may not have been logged.
Can't compare yet. Culprit has one scan in the baseline period; it needs two.
No queries ran in the recent period. No query changed by more than the threshold.
Database time per hour: 75 ms recently; nothing to compare it with before. 1 query is new.
Database time per hour fell to 68%: 1.1 s → 750 ms. 2 queries got slower, 1 query is new and 1 query got faster.
Database time per hour fell to 57%: 1.1 s → 600 ms. 1 query got slower, 2 queries are new and 1 query got faster.
Database time per hour fell to 10%: 1.0 s → 100 ms. 1 query got faster.
Database time per hour stayed about the same: 100 ms → 105 ms. No query changed by more than the threshold.
Database time fell to <1% for the time covered: 100 ms over 2.0 s before, 300 ms over 40 min since. 1 query got slower.
0.004: 4 µs
0.5: 0.50 ms
5: 5.0 ms
50: 50 ms
5000: 5.0 s
2026-10-13T14:05:00Z: Tue 14:05 UTC | Tue Oct 13 14:05 UTC
2026-01-04T08:00:00+02:00: Sun 06:00 UTC | Sun Jan 4 06:00 UTC
not a time: not a time | not a time
4.1× 41× 1.0×"#
        );
    }

    #[test]
    fn plan_switches_are_named() {
        let mut q = query("q", Some(stats(100.0, 2.0)), Some(stats(100.0, 8.6)));
        q.plans = vec![
            PlanPeriods {
                id: "815".into(),
                label: Some("Index Seek on customers".into()),
                baseline: Some(stats(100.0, 2.0)),
                recent: Some(stats(5.0, 2.0)),
                first_recent: None,
            },
            PlanPeriods {
                id: "685".into(),
                label: Some("Index Scan on customers".into()),
                baseline: None,
                recent: Some(stats(95.0, 9.0)),
                first_recent: Some("2026-10-13T14:05:00Z".into()),
            },
        ];
        let changes = compare("shop", Engine::Mssql, data(vec![q], 1.0), &CompareOptions::default());
        assert_eq!(
            changes.queries[0].headline,
            "Got 4.3× slower (2.0 ms → 8.6 ms per run) on Tue 14:05 UTC, when SQL Server switched from plan 815 to plan 685 (Index Seek on customers → Index Scan on customers)."
        );
        let mut same = query("s", Some(stats(100.0, 2.0)), Some(stats(100.0, 8.0)));
        same.plans = vec![PlanPeriods {
            id: "1".into(),
            baseline: Some(stats(100.0, 2.0)),
            recent: Some(stats(100.0, 8.0)),
            ..Default::default()
        }];
        let changes = compare("shop", Engine::Mssql, data(vec![same], 1.0), &CompareOptions::default());
        assert!(changes.queries[0].same_plan);
        assert!(changes.queries[0].headline.contains("with the same plan"));
    }
}
