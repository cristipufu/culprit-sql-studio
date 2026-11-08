//! Parameter sensitivity: a query that is fast for most calls and slow for some, usually because a plan compiled for
//! one value is reused for very different ones (SQL Server, Postgres generic plans) or because some values touch far
//! more rows (any engine). Each engine shows it through what it keeps: Query Store's plans used side by side, and
//! MySQL's run-time histogram. A single plan with wildly varying run times (all engines, including Postgres's
//! min/max/stddev) is `unstable-query`, which shares the fix ladders below.

use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::{
    Category, CheckInfo, Confidence, Engine, Finding, Fix, Metric, PlanVariant, QueryStat, Severity, Snapshot,
    SubjectKind, Unit, format, t,
};

use crate::queries::{apply_impact, slow_log_only};
use crate::support::*;

/// Enough calls that "most" and "some" mean something.
const MIN_RUNS: f64 = 50.0;
/// A plan has to carry a real share of the calls to count as one of "the" plans.
const MIN_PLAN_SHARE: f64 = 0.01;
/// How much slower the slow side must be, and by how much in absolute terms.
const MIN_FACTOR: f64 = 5.0;
const MIN_GAP_MS: f64 = 5.0;
/// The slowest runs must stand far out from the typical one.
const MIN_TAIL_FACTOR: f64 = 10.0;
const MIN_TAIL_MS: f64 = 200.0;

pub fn parameter_sensitive_info() -> CheckInfo {
    CheckInfo {
        id: "parameter-sensitive".into(),
        title: t!("check.parameter_sensitive.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Mysql],
        requires: vec![],
        description: t!("check.parameter_sensitive.info.description"),
    }
}

check!(
    ParameterSensitiveCheck: parameter_sensitive_info,
    decide = parameter_sensitive_cases,
    render = parameter_sensitive_finding,
    case<'s> = ParameterSensitive<'s>,
    evidence = &[Evidence::Query(QueryMeasure::ParameterSpread)],
);

/// How the slow calls show up in the data.
#[derive(Debug)]
enum Shape<'a> {
    /// Plans of one query side by side (Query Store): the fast ones most calls run with (`fast` the busiest of them,
    /// `fast_ms` the slowest of their averages, `fast_calls` their calls together), and a slow one still in use.
    Plans { fast: &'a PlanVariant, slow: &'a PlanVariant, fast_ms: f64, fast_calls: f64, fast_plans: usize },
    /// A latency histogram with a long tail (MySQL).
    Tail { typical_ms: f64, p99_ms: f64, slow_share: Option<f64> },
}

/// "Most calls": the fast plan carries at least this share of them, so forcing it changes little for most callers.
const MOST_CALLS: f64 = 0.5;
/// A slow plan quiet for this many of its typical intervals between runs (and a day) has stopped.
const QUIET_INTERVALS: f64 = 20.0;

fn plans_shape<'a>(snapshot: &Snapshot, q: &'a QueryStat) -> Option<Shape<'a>> {
    // A forced plan already decides for every call; what it does is tuning-advice's to say.
    if q.workload_plans().iter().any(|p| p.is_forced) {
        return None;
    }
    let busy: Vec<&PlanVariant> = q
        .workload_plans()
        .iter()
        .filter(|p| p.executions >= 10.0 && p.executions >= q.executions * MIN_PLAN_SHARE)
        .collect();
    // The slow plan: the slowest still in use. One that stopped is behind the query, but a slower one may still run.
    let slow = busy
        .iter()
        .copied()
        .filter(|p| !slow_plan_stopped(snapshot, p))
        .max_by(|a, b| a.mean_time_ms.total_cmp(&b.mean_time_ms))?;
    // The fast plans: each well ahead of it. Together they carry most calls; a rare fast plan beside a slow common one
    // isn't "fast for most calls".
    let fast_group: Vec<&PlanVariant> = busy
        .iter()
        .copied()
        .filter(|p| {
            p.id != slow.id
                && slow.mean_time_ms >= p.mean_time_ms * MIN_FACTOR
                && slow.mean_time_ms - p.mean_time_ms >= MIN_GAP_MS
        })
        .collect();
    let fast_calls: f64 = fast_group.iter().map(|p| p.executions).sum();
    if fast_calls < q.executions * MOST_CALLS {
        return None;
    }
    // SQL Server 2022's parameter-sensitive plan optimization already keeps a plan per range of values: a slower plan
    // of another variant is the one built for the values that match more rows, not a plan reused for the wrong ones.
    if slow.variant_query.is_some()
        && fast_group.iter().all(|p| p.variant_query.is_some() && p.variant_query != slow.variant_query)
    {
        return None;
    }
    let fast = fast_group.iter().copied().max_by(|a, b| a.executions.total_cmp(&b.executions))?;
    let fast_ms = fast_group.iter().map(|p| p.mean_time_ms).fold(0.0, f64::max);
    // Side by side: the slow plan ran while one of the fast ones did, so values pick the plan. A fast plan that was
    // replaced is a regression instead.
    let ran_with_slow = |p: &PlanVariant| match (
        p.last_seen.as_deref(),
        slow.first_seen.as_deref(),
        slow.last_seen.as_deref(),
        p.first_seen.as_deref(),
    ) {
        (Some(fast_last), Some(slow_first), Some(slow_last), Some(fast_first)) => {
            fast_last >= slow_first && slow_last >= fast_first
        }
        _ => true,
    };
    fast_group.iter().any(|p| ran_with_slow(p)).then_some(Shape::Plans {
        fast,
        slow,
        fast_ms,
        fast_calls,
        fast_plans: fast_group.len(),
    })
}

/// The slow plan hasn't run for a long while before the data ends: whatever made it slow is behind the query.
fn slow_plan_stopped(snapshot: &Snapshot, slow: &PlanVariant) -> bool {
    let end = snapshot.workload.as_ref().and_then(|w| w.window_end.as_deref()).and_then(parse_epoch_seconds);
    let (Some(end), Some(last)) = (end, slow.last_seen.as_deref().and_then(parse_epoch_seconds)) else {
        return false;
    };
    let first = slow.first_seen.as_deref().and_then(parse_epoch_seconds).unwrap_or(last);
    let interval = (last - first).max(1.0) / slow.executions.max(1.0);
    // Stopped as statements stop: out of its rhythm, and quiet for a day ([`crate::refine::silent_long_enough`]).
    end - last >= interval * QUIET_INTERVALS && crate::refine::silent_long_enough(end - last)
}

/// Runs in the slow tail the shape needs: with fewer, "1 in 100 runs" is one or two runs.
pub(crate) const MIN_TAIL_RUNS: f64 = 3.0;

fn tail_shape<'a>(snapshot: &Snapshot, q: &'a QueryStat) -> Option<Shape<'a>> {
    let spread = q.spread.as_ref()?;
    let typical = spread.p50_ms.or(spread.p95_ms.map(|p| p.min(q.mean_time_ms)))?;
    let p99 = spread.p99_ms?;
    if p99 < typical.max(0.05) * MIN_TAIL_FACTOR || p99 < MIN_TAIL_MS {
        return None;
    }
    let threshold = typical * MIN_TAIL_FACTOR;
    let total: f64 = spread.histogram.iter().map(|b| b[2]).sum();
    let slow: f64 = spread.histogram.iter().filter(|b| b[0] >= threshold).map(|b| b[2]).sum();
    // The tail has to be several runs: a histogram counts them; without one, a hundredth of the runs must be.
    let enough = if total > 0.0 { slow >= MIN_TAIL_RUNS } else { q.executions * 0.01 >= MIN_TAIL_RUNS };
    if !enough {
        return None;
    }
    // performance_schema's histogram counts from the server's start: within a day of it a few slow runs may be the first
    // ones, on a cold cache, and the tail needs more of them to be the values'. The slow query log dates each run.
    let young = snapshot.server.engine == Engine::Mysql
        && snapshot.workload.as_ref().is_some_and(|w| w.kind() == Some(culprit_core::WorkloadKind::PerformanceSchema))
        && counter_days(snapshot).is_none_or(|d| d < 1.0);
    if young && slow < 10.0 * MIN_TAIL_RUNS {
        return None;
    }
    let slow_share = (total > 0.0 && slow > 0.0).then(|| slow / total);
    Some(Shape::Tail { typical_ms: typical, p99_ms: p99, slow_share })
}

/// A statement whose run time depends on its values, and what its data establishes.
pub struct ParameterSensitive<'s> {
    query: &'s QueryStat,
    shape: Shape<'s>,
    impact: Option<QueryImpact>,
    severity: Severity,
    score: f64,
    /// It stopped running: told in the past.
    stopped: bool,
    /// The plan most calls run fast with on its own, the one forcing is offered for.
    most_calls: Option<&'s PlanVariant>,
    /// What a long tail's source keeps of the runs.
    tail: Option<TailSource>,
    fix: Ladder,
}

/// What a run-time tail is read from.
struct TailSource {
    /// The slow query log keeps only runs over long_query_time: its "typical" run is a typical slow one.
    logged: bool,
    /// The workload is the slow query log's.
    from_log: bool,
    /// long_query_time, where the scan read it (ms).
    threshold_ms: Option<f64>,
    /// 99 in 100 is said only of a hundred runs or more.
    percentile_99: bool,
    /// The share of its time spent waiting for locks.
    lock_share: f64,
}

/// The fix ladder that applies.
enum Ladder {
    Mssql(MssqlLadder),
    Mysql { lock_share: f64, slow_runs: Option<f64> },
}

/// The six statements whose values cost the most.
fn parameter_sensitive_cases(snapshot: &Snapshot) -> Vec<ParameterSensitive<'_>> {
    let Some(workload) = snapshot.workload.as_ref() else { return Vec::new() };
    let mut cases: Vec<ParameterSensitive> = app_queries(snapshot)
        .filter(|q| q.executions >= MIN_RUNS)
        .filter_map(|q| {
            let shape = plans_shape(snapshot, q).or_else(|| tail_shape(snapshot, q))?;
            // Its share as every surface states it (over Culprit's scans where they measured it).
            let share = workload.statement_share(q);
            let slow_time_share = match &shape {
                Shape::Plans { slow, .. } => workload.share_of(slow.total_time_ms, q),
                _ => share,
            };
            (share >= 0.01 || slow_time_share >= 0.01).then(|| parameter_sensitive_case(snapshot, q, shape, share))
        })
        .collect();
    cases.sort_by(|a, b| b.score.total_cmp(&a.score));
    cases.truncate(6);
    cases
}

fn parameter_sensitive_case<'s>(
    snapshot: &'s Snapshot,
    q: &'s QueryStat,
    shape: Shape<'s>,
    share: f64,
) -> ParameterSensitive<'s> {
    // Graded on what the slow calls cost beyond running at the fast speed, not on the whole statement's time: the
    // fast calls are fine as they are.
    let extra = match &shape {
        Shape::Plans { slow, fast_ms, .. } => (slow.mean_time_ms - fast_ms).max(0.0) * slow.executions,
        // Each run in the tail is at least MIN_TAIL_FACTOR times the typical one.
        Shape::Tail { typical_ms, slow_share, .. } => {
            slow_share.unwrap_or(0.01) * q.executions * typical_ms * (MIN_TAIL_FACTOR - 1.0)
        }
    };
    let extra_share = snapshot.workload.as_ref().map_or(0.0, |w| w.share_of(extra, q));
    let (most_calls, tail, fix) = match &shape {
        Shape::Plans { fast, .. } => {
            // Forcing is offered only for a plan that serves most calls on its own.
            let most_calls = (fast.executions >= q.executions * MOST_CALLS).then_some(*fast);
            (most_calls, None, Ladder::Mssql(MssqlLadder::of(snapshot, q, most_calls, extra)))
        }
        Shape::Tail { slow_share, .. } => {
            let locks = q.spread.as_ref().and_then(|s| s.lock_ms).filter(|l| *l > 0.0);
            let lock_share = locks.map(|l| (l / q.total_time_ms.max(0.001)).clamp(0.0, 1.0)).unwrap_or(0.0);
            let tail = TailSource {
                logged: slow_log_only(snapshot),
                from_log: snapshot
                    .workload
                    .as_ref()
                    .is_some_and(|w| w.kind() == Some(culprit_core::WorkloadKind::SlowLog)),
                threshold_ms: snapshot
                    .setting("long_query_time")
                    .and_then(|s| s.value.trim().parse::<f64>().ok())
                    .map(|s| s * 1000.0),
                percentile_99: q.executions >= 100.0,
                lock_share,
            };
            let slow_runs = slow_share.map(|s| s * q.executions);
            (None, Some(tail), Ladder::Mysql { lock_share, slow_runs })
        }
    };
    ParameterSensitive {
        query: q,
        impact: query_impact(snapshot, &[q]),
        severity: severity_for_time_of(snapshot, &[q], extra, extra_share),
        score: score_for_share(extra_share.max(share * 0.1)),
        stopped: crate::all_stopped(snapshot, &[q]),
        shape,
        most_calls,
        tail,
        fix,
    }
}

fn compiled(plan: &PlanVariant, q: &QueryStat) -> Option<String> {
    let raw = plan.compiled.clone().or_else(|| {
        let dominant = q.workload_plans().first()?;
        (dominant.id == plan.id).then(|| q.plan.as_ref()?.facts().compiled_parameters.clone()).flatten()
    })?;
    let tidy = tidy_parameters(&raw);
    (!tidy.is_empty()).then_some(tidy)
}

/// Showplan writes compiled values as `@t = (2)` and lists output parameters as `@c = NULL`; keep what was sniffed.
pub(crate) fn tidy_parameters(raw: &str) -> String {
    raw.split(", @")
        .enumerate()
        .map(|(i, p)| if i == 0 { p.to_string() } else { format!("@{p}") })
        .filter_map(|p| {
            let (name, value) = p.split_once(" = ")?;
            let value = value.trim();
            if value.eq_ignore_ascii_case("NULL") {
                return None;
            }
            let value = value.strip_prefix('(').and_then(|v| v.strip_suffix(')')).unwrap_or(value);
            Some(format!("{name} = {value}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn parameter_sensitive_finding(snapshot: &Snapshot, case: ParameterSensitive<'_>) -> Finding {
    let q = case.query;
    let handle = capitalize(&query_handle(q));
    let mut f = finding("parameter-sensitive", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    if let Some(impact) = &case.impact {
        apply_impact(snapshot, &mut f, impact);
    }
    f.severity = case.severity;
    f.impact.score = case.score;
    // A statement that stopped running is told in the past.
    let tense = if case.stopped { "past" } else { "present" };
    let run = culprit_core::changes::run_time;
    match &case.shape {
        Shape::Plans { fast, slow, fast_ms, fast_calls, fast_plans } => {
            let slow_share = slow.executions / q.executions.max(1.0);
            let calls = |n: f64| format::percent(n / q.executions.max(1.0));
            // One plan for most calls, or several, each well ahead of the slow one: then the slowest of them is the bound.
            f.title = if *fast_plans == 1 {
                t!(
                    "check.parameter_sensitive.title.plans",
                    query = &handle,
                    tense = tense,
                    fast = run(fast.mean_time_ms),
                    slow = run(slow.mean_time_ms),
                    share = format::percent(slow_share)
                )
            } else {
                t!(
                    "check.parameter_sensitive.title.plans_or_less",
                    query = &handle,
                    tense = tense,
                    fast = run(*fast_ms),
                    slow = run(slow.mean_time_ms),
                    share = format::percent(slow_share)
                )
            };
            f.impact.label =
                t!("check.parameter_sensitive.impact.calls", factor = times(slow.mean_time_ms / fast_ms.max(0.001)));
            let compiled_for = |p: &PlanVariant| {
                compiled(p, q).map(|c| t!("check.parameter_sensitive.compiled_for", values = c)).unwrap_or_default()
            };
            let plans = if q.workload_plans().len() == 2 {
                t!("check.parameter_sensitive.two")
            } else {
                format::count(q.workload_plans().len() as f64)
            };
            let first = if *fast_plans == 1 {
                t!(
                    "check.parameter_sensitive.summary.plans",
                    plans = plans,
                    fast = &fast.id,
                    fast_calls = calls(*fast_calls),
                    fast_ms = run(fast.mean_time_ms),
                    fast_compiled = compiled_for(fast),
                    slow = &slow.id,
                    slow_ms = run(slow.mean_time_ms),
                    slow_compiled = compiled_for(slow),
                    slow_calls = format::count(slow.executions)
                )
            } else {
                t!(
                    "check.parameter_sensitive.summary.plans_several",
                    plans = plans,
                    fast_plans = fast_plans.to_string(),
                    fast_ms = run(*fast_ms),
                    fast_calls = calls(*fast_calls),
                    busiest = &fast.id,
                    busiest_calls = calls(fast.executions),
                    busiest_ms = run(fast.mean_time_ms),
                    busiest_compiled = compiled_for(fast),
                    slow = &slow.id,
                    slow_ms = run(slow.mean_time_ms),
                    slow_compiled = compiled_for(slow),
                    slow_calls = format::count(slow.executions)
                )
            };
            f.summary = [first, t!("check.parameter_sensitive.summary.plans_why")].join(" ");
            f.metrics.push(Metric::new(t!("common.metric.fast_plan"), fast.mean_time_ms, Unit::Ms));
            f.metrics.push(Metric::new(t!("check.parameter_sensitive.metric.slow_plan"), slow.mean_time_ms, Unit::Ms));
            f.metrics.push(Metric::new(t!("check.parameter_sensitive.metric.slow_calls"), slow_share, Unit::Ratio));
            // Forcing is offered only for a plan that serves most calls on its own.
            if let Some(p) = case.most_calls {
                f.metrics.push(Metric::text(t!("check.parameter_sensitive.metric.most_calls_plan"), p.id.clone()));
                f.facts.usual_fast_plan = Some(p.id.clone());
            }
            f.confidence = Confidence::High;
        }
        Shape::Tail { typical_ms, p99_ms, slow_share } => {
            // The slow query log keeps only runs over long_query_time: its "typical" run is a typical slow one.
            let tail = case.tail.as_ref().expect("a tail's facts");
            let (typical, query) = (run(*typical_ms), query_handle(q));
            f.title = match (slow_share, tail.logged) {
                (Some(s), false) if *s >= 0.001 => t!(
                    "check.parameter_sensitive.title.tail",
                    query = &handle,
                    tense = tense,
                    typical = typical.clone(),
                    share = format::percent(*s),
                    slow = run(typical_ms * MIN_TAIL_FACTOR)
                ),
                (Some(s), true) if *s >= 0.001 => t!(
                    "check.parameter_sensitive.title.tail_logged",
                    query = &query,
                    tense = tense,
                    typical = typical.clone(),
                    share = format::percent(*s),
                    slow = run(typical_ms * MIN_TAIL_FACTOR)
                ),
                (_, false) => t!(
                    "check.parameter_sensitive.title.tail_p99",
                    query = &handle,
                    tense = tense,
                    typical = typical.clone(),
                    p99 = run(*p99_ms)
                ),
                (_, true) => t!(
                    "check.parameter_sensitive.title.tail_p99_logged",
                    query = &query,
                    tense = tense,
                    typical = typical.clone(),
                    p99 = run(*p99_ms)
                ),
            };
            f.impact.label =
                t!("check.parameter_sensitive.impact.runs", factor = times(p99_ms / typical_ms.max(0.001)));
            let name = snapshot.server.name();
            let source = match (tail.from_log && !tail.logged, tail.logged, tail.threshold_ms) {
                (true, _, _) => t!("check.parameter_sensitive.source.log_every_run"),
                (false, true, Some(ms)) => {
                    t!("check.parameter_sensitive.source.log_kept_over", threshold = format::duration(ms))
                }
                (false, true, None) => t!("check.parameter_sensitive.source.log_kept"),
                (false, false, _) => t!("check.parameter_sensitive.source.histogram", server = name),
            };
            let slowest = q.max_time_ms.map(run).unwrap_or_else(|| t!("check.parameter_sensitive.slowest_longer"));
            let mut summary = vec![
                if tail.percentile_99 {
                    t!(
                        "check.parameter_sensitive.summary.tail_p99",
                        source = source,
                        typical = typical,
                        p99 = run(*p99_ms),
                        slowest = slowest
                    )
                } else {
                    t!("check.parameter_sensitive.summary.tail", source = source, typical = typical, slowest = slowest)
                },
                t!("check.parameter_sensitive.summary.tail_why", server = name),
            ];
            if tail.lock_share >= 0.2 {
                summary
                    .push(t!("check.parameter_sensitive.summary.tail_locks", share = format::percent(tail.lock_share)));
            }
            f.summary = summary.join(" ");
            f.metrics.push(Metric::new(
                if tail.logged {
                    t!("check.parameter_sensitive.metric.typical_logged_run")
                } else {
                    t!("check.parameter_sensitive.metric.typical_run")
                },
                *typical_ms,
                Unit::Ms,
            ));
            if tail.percentile_99 {
                f.metrics.push(Metric::new(t!("check.parameter_sensitive.metric.p99"), *p99_ms, Unit::Ms));
            }
            if let Some(s) = slow_share {
                f.metrics.push(Metric::new(t!("check.parameter_sensitive.metric.slow_runs"), *s, Unit::Ratio));
            }
            f.confidence = if tail.logged { Confidence::Medium } else { Confidence::High };
        }
    }
    f.fix = Some(match &case.fix {
        Ladder::Mssql(ladder) => ladder.fix(snapshot, q),
        Ladder::Mysql { lock_share, slow_runs } => mysql_fix(snapshot, q, *lock_share, *slow_runs),
    });
    f.queries = query_refs(&[q]);
    f.verify = vec![t!("check.parameter_sensitive.verify.compare"), t!("check.parameter_sensitive.verify.rescan")];
    f
}

/// The fix ladder for SQL Server, lightest first, with only the steps this server has: Query Store hints and
/// parameter-sensitive plan optimization came with SQL Server 2022 (Azure SQL has both), and the optimization needs
/// compatibility level 160. Forcing is offered only for a plan most calls already run with. RECOMPILE is ranked by what
/// it costs: a compile on every call (Query Store's average compile, else the plan's own CompileTime) against the
/// `slow_extra_ms` the slow calls cost beyond the fast ones; where compiling every run of the window costs more, it comes
/// last and says so.
pub(crate) struct MssqlLadder {
    /// The rungs this server has, lightest first.
    steps: Vec<Step>,
    /// The plan to force, where one serves most calls on its own.
    force: Option<String>,
    /// A compile, and one on every run of the window, where known.
    compile_ms: Option<f64>,
    recompile_ms: Option<f64>,
    /// Compiling every run costs more than the slow calls do: RECOMPILE comes last.
    costly: bool,
    slow_extra_ms: f64,
}

/// One rung of SQL Server's ladder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Force the plan most calls run fast with.
    Force,
    /// Plan each call for its own values: a Query Store hint, or the application's OPTION (RECOMPILE).
    Recompile { hint: bool },
    /// Let SQL Server keep a plan per range of values.
    Psp,
    /// Plan for the typical value, in the application.
    Typical,
}

impl MssqlLadder {
    pub(crate) fn of(snapshot: &Snapshot, q: &QueryStat, fast: Option<&PlanVariant>, slow_extra_ms: f64) -> Self {
        let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        let server = &snapshot.server;
        let azure = server.platform().is_some_and(|p| p.is_azure_sql());
        let sql2022 = azure || culprit_core::version_at_least(&server.version, [16, 0, 0]);
        let compat = snapshot.setting("compatibility_level").and_then(|s| s.value.trim().parse::<u32>().ok());
        let psp_on = snapshot
            .setting("PARAMETER_SENSITIVE_PLAN_OPTIMIZATION")
            .is_some_and(|s| matches!(s.value.trim().to_ascii_uppercase().as_str(), "1" | "ON"));
        let mut steps = Vec::new();
        let force = fast.filter(|p| numeric(&q.id) && numeric(&p.id) && p.executions >= q.executions * MOST_CALLS);
        if force.is_some() {
            steps.push(Step::Force);
        }
        // A compile, by Query Store's average over its compiles, else the one the plan records.
        let compile_ms = q
            .costs
            .as_ref()
            .and_then(|c| Some(c.plan_ms? / c.plans.filter(|n| *n > 0.0)?))
            .or_else(|| q.plan.as_ref()?.facts().compile_ms)
            .filter(|ms| *ms > 0.0);
        let recompile_ms = compile_ms.map(|ms| ms * q.executions);
        let costly = recompile_ms.is_some_and(|total| slow_extra_ms > 0.0 && total >= slow_extra_ms);
        let recompile = Step::Recompile { hint: numeric(&q.id) && sql2022 };
        if !costly {
            steps.push(recompile);
        }
        if sql2022 && !psp_on && compat.is_none_or(|c| c >= 160) {
            steps.push(Step::Psp);
        }
        steps.push(Step::Typical);
        if costly {
            steps.push(recompile);
        }
        MssqlLadder { steps, force: force.map(|p| p.id.clone()), compile_ms, recompile_ms, costly, slow_extra_ms }
    }

    /// The ladder in words, its SQL each rung's.
    pub(crate) fn fix(&self, snapshot: &Snapshot, q: &QueryStat) -> Fix {
        let sql: Vec<String> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                // (what it does, the SQL or the change to make): comments of the fix's SQL.
                let (what, sql) = match step {
                    Step::Force => (
                        "Keep the plan most calls run fast with, for every call (no code change; undo with sp_query_store_unforce_plan)".to_string(),
                        format!(
                            "EXEC sp_query_store_force_plan @query_id = {}, @plan_id = {};",
                            q.id,
                            self.force.as_deref().unwrap_or_default()
                        ),
                    ),
                    Step::Recompile { hint: true } => (
                        "Plan each call for its own values (costs compile CPU per call)".to_string(),
                        format!(
                            "EXEC sys.sp_query_store_set_hints @query_id = {}, @query_hints = N'OPTION (RECOMPILE)';",
                            q.id
                        ),
                    ),
                    Step::Recompile { hint: false } => (
                        "Plan each call for its own values (costs compile CPU per call), in the application".to_string(),
                        "--    append OPTION (RECOMPILE) to the statement".to_string(),
                    ),
                    Step::Psp => (
                        "Let SQL Server keep a plan per range of values".into(),
                        "ALTER DATABASE SCOPED CONFIGURATION SET PARAMETER_SENSITIVE_PLAN_OPTIMIZATION = ON;".into(),
                    ),
                    Step::Typical => (
                        "In the application, plan for the typical value instead of the first one seen".into(),
                        "--    append OPTION (OPTIMIZE FOR UNKNOWN) to the statement".into(),
                    ),
                };
                let what = if i == 0 { what } else { format!("Or {}{}", what[..1].to_lowercase(), &what[1..]) };
                format!("-- {}. {what}:\n{sql}", i + 1)
            })
            .collect();
        let options: Vec<String> = self
            .steps
            .iter()
            .map(|step| match step {
                Step::Force => t!("check.parameter_sensitive.option.force"),
                Step::Recompile { .. } => t!("check.parameter_sensitive.option.recompile"),
                Step::Psp => t!("check.parameter_sensitive.option.psp"),
                Step::Typical => t!("check.parameter_sensitive.option.typical"),
            })
            .collect();
        // What a compile on every call costs, against what the slow calls cost, where both are known.
        let recompile_cost = match (self.compile_ms, self.recompile_ms) {
            (Some(each), Some(total)) if self.costly => t!(
                "check.parameter_sensitive.cost.recompile_last",
                each = format::duration(each),
                total = format::duration(total),
                runs = format::count(q.executions),
                window = snapshot
                    .workload
                    .as_ref()
                    .map(|w| w.window_phrase())
                    .unwrap_or_else(|| t!("check.parameter_sensitive.cost.the_window")),
                slow = format::duration(self.slow_extra_ms)
            ),
            (Some(each), _) => t!("check.parameter_sensitive.cost.recompile_each", each = format::duration(each)),
            _ => t!("check.parameter_sensitive.cost.recompile"),
        };
        let cost = if self.force.is_some() {
            let mut cost = vec![t!("check.parameter_sensitive.cost.force")];
            cost.extend(crate::tuning::hand_forcing_note(snapshot));
            cost.push(recompile_cost);
            cost.join(" ")
        } else {
            recompile_cost
        };
        Fix {
            summary: t!("check.parameter_sensitive.fix.ladder", options = or_list(&options)),
            sql: Some(sql.join("\n\n")),
            cost: Some(cost),
        }
    }
}

/// [`MssqlLadder`] decided and worded at once, for a check that words a fix where it decides it.
pub(crate) fn mssql_fix(snapshot: &Snapshot, q: &QueryStat, fast: Option<&PlanVariant>, slow_extra_ms: f64) -> Fix {
    MssqlLadder::of(snapshot, q, fast, slow_extra_ms).fix(snapshot, q)
}

/// "a, b or c".
fn or_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => t!("check.parameter_sensitive.or_list", items = rest.join(", "), last = last),
    }
}

pub(crate) fn postgres_fix() -> Fix {
    Fix {
        summary: t!("check.parameter_sensitive.fix.postgres"),
        sql: Some(
            "-- Compare a slow and a fast value (outside production, ANALYZE runs the statement):\nEXPLAIN (ANALYZE, BUFFERS) <the statement with a slow value>;\n\n-- Plan every call for its own values (for the role the application connects as):\nALTER ROLE <app_role> SET plan_cache_mode = force_custom_plan;\n\n-- Finer statistics for an uneven column, then refresh them:\nALTER TABLE <table> ALTER COLUMN <column> SET STATISTICS 1000;\nANALYZE <table>;".into(),
        ),
        cost: None,
    }
}

/// The fix ladder for MySQL and MariaDB. Slow runs that mostly wait for row locks (`lock_share` of the statement's time,
/// in `slow_runs` runs where the histogram counts them) are about what holds the locks, found the way every check that
/// names a lock wait finds it ([`find_lock_holder`]).
pub(crate) fn mysql_fix(snapshot: &Snapshot, q: &QueryStat, lock_share: f64, slow_runs: Option<f64>) -> Fix {
    let mariadb = snapshot.server.is_mariadb();
    if lock_share >= 0.2 {
        let LockHolder { lead, sql } = find_lock_holder(snapshot, q, slow_runs);
        return Fix {
            summary: [t!("check.parameter_sensitive.fix.locks"), lead, t!("check.parameter_sensitive.fix.locks_short")]
                .join(" "),
            sql,
            cost: None,
        };
    }
    if mariadb {
        return Fix {
            summary: t!("check.parameter_sensitive.fix.mariadb"),
            sql: Some("-- Collect a histogram for an uneven column (used with the default use_stat_tables):\nANALYZE TABLE <table> PERSISTENT FOR COLUMNS (<column>) INDEXES ();".into()),
            cost: None,
        };
    }
    Fix {
        summary: t!("check.parameter_sensitive.fix.mysql"),
        sql: Some("-- Give the optimizer the distribution of an uneven column:\nANALYZE TABLE <table> UPDATE HISTOGRAM ON <column> WITH 64 BUCKETS;".into()),
        cost: None,
    }
}

fn times(factor: f64) -> String {
    if factor >= 10.0 { format!("{}×", factor.round()) } else { format!("{:.1}×", factor).replace(".0×", "×") }
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
    use crate::support::tests::sample;

    fn plan(id: &str, runs: f64, mean: f64) -> PlanVariant {
        let mut p: PlanVariant = serde_json::from_value(serde_json::json!({
            "id": id, "executions": runs, "total_time_ms": runs * mean, "mean_time_ms": mean,
            "first_seen": null, "last_seen": null, "is_forced": false
        }))
        .expect("plan variant");
        p.compiled = Some(format!("@P1 = ({id})"));
        p
    }

    fn mssql() -> Snapshot {
        crate::waits::snapshot_with(Engine::Mssql)
    }

    #[test]
    fn two_plans_one_slow_is_sensitivity() {
        let s = mssql();
        let mut q = sample();
        q.executions = 1000.0;
        q.plans = vec![plan("815", 980.0, 2.0), plan("685", 20.0, 3400.0)];
        match plans_shape(&s, &q) {
            Some(Shape::Plans { fast, slow, .. }) => {
                assert_eq!((fast.id.as_str(), slow.id.as_str()), ("815", "685"))
            }
            other => panic!("{other:?}"),
        }
        q.plans = vec![plan("1", 500.0, 2.0), plan("2", 500.0, 3.0)];
        assert!(plans_shape(&s, &q).is_none(), "1.5× apart is not two different worlds");
    }

    #[test]
    fn fast_plans_together_carry_most_calls_and_the_slow_one_still_runs() {
        // As on a real Azure SQL database: no single fast plan has half the calls, the slowest plan stopped days ago,
        // and a slow one ran half an hour before the data ends, while a fast one kept running.
        let mut s = mssql();
        let w = s.workload.as_mut().expect("workload");
        w.window_end = Some("2026-10-14T15:21:00Z".into());
        let mut q = sample();
        q.executions = 27_000.0;
        let at = |mut p: PlanVariant, first: &str, last: &str| {
            p.first_seen = Some(first.into());
            p.last_seen = Some(last.into());
            p
        };
        q.plans = vec![
            at(plan("2485", 11_000.0, 1.2), "2026-10-12T13:54:00Z", "2026-10-14T12:58:00Z"),
            at(plan("2179", 7_200.0, 0.1), "2026-10-11T16:29:00Z", "2026-10-12T13:37:00Z"),
            at(plan("3020", 3_700.0, 0.15), "2026-10-13T17:06:00Z", "2026-10-14T15:16:00Z"),
            at(plan("1944", 1_200.0, 27.0), "2026-10-10T12:54:00Z", "2026-10-12T11:47:00Z"),
            at(plan("3312", 1_240.0, 19.0), "2026-10-14T14:32:00Z", "2026-10-14T14:49:00Z"),
        ];
        match plans_shape(&s, &q) {
            Some(Shape::Plans { fast, slow, fast_plans, fast_calls, .. }) => {
                assert_eq!((fast.id.as_str(), slow.id.as_str(), fast_plans), ("2485", "3312", 3));
                assert!(fast_calls > q.executions * MOST_CALLS);
            }
            other => panic!("{other:?}"),
        }
        let case = parameter_sensitive_case(&s, &q, plans_shape(&s, &q).unwrap(), 0.04);
        // No plan serves most calls on its own: forcing one isn't offered.
        assert!(case.most_calls.is_none());
        let f = parameter_sensitive_finding(&s, case);
        assert!(f.title.contains("1.2 ms or less for most calls"), "{}", f.title);
        assert!(f.facts.usual_fast_plan.is_none(), "{:?}", f.metrics);
    }

    #[test]
    fn the_fast_plan_has_to_carry_most_calls() {
        // 10 runs at 5 ms beside 290 at 60 ms: most calls are slow, so "fast for most calls" would be false, and
        // forcing the rare plan is the classic sniffing trap.
        let s = mssql();
        let mut q = sample();
        q.executions = 300.0;
        q.plans = vec![plan("1", 290.0, 60.0), plan("2", 10.0, 5.0)];
        assert!(plans_shape(&s, &q).is_none());
    }

    #[test]
    fn a_forced_plan_or_a_slow_plan_that_stopped_is_not_sensitivity() {
        let s = mssql();
        let mut q = sample();
        q.executions = 1000.0;
        q.plans = vec![plan("815", 980.0, 2.0), plan("685", 20.0, 3400.0)];
        q.plans[0].is_forced = true;
        assert!(plans_shape(&s, &q).is_none(), "a forced plan already decides");
        q.plans[0].is_forced = false;
        // The demo's data ends at 12:13 on Oct 11; the slow plan last ran at 08:00 the day before, after 20 runs in two
        // hours.
        q.plans[1].first_seen = Some("2026-10-10T06:00:00Z".into());
        q.plans[1].last_seen = Some("2026-10-10T08:00:00Z".into());
        assert!(plans_shape(&s, &q).is_none(), "the slow plan stopped running");
        // Quiet since 08:00 the same day: four hours are too soon to say it stopped.
        q.plans[1].first_seen = Some("2026-10-11T06:00:00Z".into());
        q.plans[1].last_seen = Some("2026-10-11T08:00:00Z".into());
        assert!(plans_shape(&s, &q).is_some(), "four quiet hours are not a stop");
        q.plans[1].last_seen = Some("2026-10-11T12:00:00Z".into());
        assert!(plans_shape(&s, &q).is_some());
    }

    #[test]
    fn the_fix_ladder_offers_what_the_server_has() {
        let mut s = mssql();
        let mut q = sample();
        q.id = "42".into();
        q.executions = 1000.0;
        let fast = plan("815", 980.0, 2.0);
        // SQL Server 2022 with parameter-sensitive plan optimization already on (the demo).
        let sql = mssql_fix(&s, &q, Some(&fast), 0.0).sql.unwrap();
        assert!(sql.starts_with("-- 1. Keep the plan most calls run fast with"), "{sql}");
        assert!(sql.contains("sp_query_store_set_hints"), "{sql}");
        assert!(!sql.contains("PARAMETER_SENSITIVE_PLAN_OPTIMIZATION"), "it is already on: {sql}");
        // SQL Server 2019: no Query Store hints, no parameter-sensitive plans.
        s.server.version = "15.0.4355.3".into();
        let sql = mssql_fix(&s, &q, Some(&fast), 0.0).sql.unwrap();
        assert!(!sql.contains("sp_query_store_set_hints") && !sql.contains("PARAMETER_SENSITIVE"), "{sql}");
        assert!(sql.contains("-- 2. Or plan each call for its own values (costs compile CPU per call), in the application:\n--    append OPTION (RECOMPILE)"), "{sql}");
        // A plan that ran for a minority of calls is never the one to force.
        let rare = plan("2", 100.0, 2.0);
        assert!(!mssql_fix(&s, &q, Some(&rare), 0.0).sql.unwrap().contains("sp_query_store_force_plan"));
    }

    // The SQL Server demo's fresh reading: OPTION (RECOMPILE) offered first for a 2 ms read whose compiles take 33 ms
    // each, when compiling every one of its 8K runs costs minutes against the 6.6 s its slow runs added.
    #[test]
    fn recompile_is_ranked_by_what_a_compile_on_every_call_costs() {
        let s = mssql();
        let mut q = sample();
        q.id = "10".into();
        (q.executions, q.mean_time_ms) = (8_000.0, 2.0);
        q.costs = Some(culprit_core::QueryCosts { plan_ms: Some(132.0), plans: Some(4.0), ..Default::default() });
        let costly = mssql_fix(&s, &q, None, 6_600.0);
        let sql = costly.sql.unwrap();
        assert!(sql.starts_with("-- 1. In the application, plan for the typical value"), "{sql}");
        assert!(sql.contains("-- 2. Or plan each call for its own values"), "{sql}");
        assert!(
            costly
                .summary
                .ends_with("plan for the typical value or plan per call for this statement only. Each is reversible."),
            "{}",
            costly.summary
        );
        let cost = costly.cost.unwrap();
        assert!(
            cost.contains("about 33 ms each")
                && cost.contains("more than the 6.6 s its slow calls cost, so it comes last"),
            "{cost}"
        );
        // Where the slow calls cost more than compiling every run, it keeps its place.
        let cheap = mssql_fix(&s, &q, None, 600_000.0);
        assert!(cheap.sql.unwrap().starts_with("-- 1. Plan each call for its own values"));
        assert_eq!(cheap.cost.as_deref(), Some("RECOMPILE spends a compile on every call (about 33 ms each)."));
    }

    #[test]
    fn a_replaced_plan_is_a_regression_not_sensitivity() {
        let s = mssql();
        let mut q = sample();
        q.executions = 1000.0;
        let mut old = plan("1", 500.0, 2.0);
        old.first_seen = Some("2026-10-10T00:00:00Z".into());
        old.last_seen = Some("2026-10-10T12:00:00Z".into());
        let mut new = plan("2", 500.0, 300.0);
        new.first_seen = Some("2026-10-10T13:00:00Z".into());
        new.last_seen = Some("2026-10-11T00:00:00Z".into());
        q.plans = vec![old, new];
        assert!(plans_shape(&s, &q).is_none());
    }

    #[test]
    fn a_tail_of_one_or_two_runs_is_not_a_shape() {
        let mut q = sample();
        q.executions = 80.0;
        q.spread = Some(culprit_core::Spread {
            p50_ms: Some(2.0),
            p99_ms: Some(900.0),
            histogram: vec![[0.0, 2.0, 60.0], [2.0, 4.0, 18.0], [500.0, 1000.0, 2.0]],
            ..Default::default()
        });
        assert!(tail_shape(&mssql(), &q).is_none(), "two slow runs");
        q.spread.as_mut().unwrap().histogram[2][2] = 3.0;
        assert!(tail_shape(&mssql(), &q).is_some());
        // Without a histogram, a hundredth of the runs has to be three runs.
        q.spread.as_mut().unwrap().histogram.clear();
        assert!(tail_shape(&mssql(), &q).is_none());
        q.executions = 400.0;
        assert!(tail_shape(&mssql(), &q).is_some());
    }

    // MySQL's histogram counts from the server's start: three slow runs within a day of it may be the cold first ones.
    #[test]
    fn a_tail_right_after_a_start_needs_more_runs() {
        let mut q = sample();
        q.executions = 100.0;
        q.spread = Some(culprit_core::Spread {
            p50_ms: Some(2.0),
            p99_ms: Some(900.0),
            histogram: vec![[0.0, 2.0, 80.0], [2.0, 4.0, 17.0], [500.0, 1000.0, 3.0]],
            ..Default::default()
        });
        let mut mysql = crate::waits::snapshot_with(Engine::Mysql);
        mysql.pressure.get_or_insert_with(Default::default).uptime_hours = Some(2.0);
        assert!(tail_shape(&mysql, &q).is_none());
        mysql.pressure.as_mut().unwrap().uptime_hours = Some(72.0);
        assert!(tail_shape(&mysql, &q).is_some());
    }

    #[test]
    fn compiled_values_read_like_values() {
        assert_eq!(tidy_parameters("@c = NULL, @t = (2)"), "@t = 2");
        assert_eq!(tidy_parameters("@P1 = N'abc', @P2 = (42)"), "@P1 = N'abc', @P2 = 42");
    }

    #[test]
    fn histogram_tail_counts_the_slow_runs() {
        let mut q = sample();
        q.spread = Some(culprit_core::Spread {
            p50_ms: Some(2.0),
            p99_ms: Some(900.0),
            histogram: vec![[0.0, 2.0, 600.0], [2.0, 4.0, 380.0], [500.0, 1000.0, 20.0]],
            ..Default::default()
        });
        match tail_shape(&mssql(), &q) {
            Some(Shape::Tail { slow_share: Some(s), .. }) => assert!((s - 0.02).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
    }
}
