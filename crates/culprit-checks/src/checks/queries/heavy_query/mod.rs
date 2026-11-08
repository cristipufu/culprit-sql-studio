//! The `heavy-query` check: Single queries that take a large share of database time, with what their plan spends it on.

mod fix;
mod idle;
mod rewrite;
#[cfg(test)]
mod tests;

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::{key, t};
use fix::*;
pub(super) use idle::idle_time;
use idle::{DominantWait, Idle, NOTHING_READS, WaitOn, dominant_wait};
pub(crate) use idle::{IdleKind, idle_kind};
pub(crate) use rewrite::late_filter_rewrite;
use rewrite::{Narrowing, narrowed_later_fix, narrowing_above};

pub fn heavy_query_info() -> CheckInfo {
    CheckInfo {
        id: "heavy-query".into(),
        title: t!("check.heavy_query.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql, Engine::Sqlite],
        requires: vec![],
        description: t!("check.heavy_query.info.description"),
    }
}

check!(
    HeavyQueryCheck: heavy_query_info,
    decide = heavy_queries,
    render = heavy_query_case,
    case<'s> = HeavyQuery<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Work)],
);

/// The heavy-query findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn heavy_query(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&HeavyQueryCheck, snapshot)
}

/// A heavy statement and what its finding concludes.
pub struct HeavyQuery<'a> {
    query: &'a QueryStat,
    case: HeavyCase<'a>,
}

fn heavy_query_case<'s>(snapshot: &'s Snapshot, heavy: HeavyQuery<'s>) -> Finding {
    render(snapshot, heavy.query, &heavy.case)
}

/// The statements heavy enough for a finding of their own, each with what it concludes.
fn heavy_queries(snapshot: &Snapshot) -> Vec<HeavyQuery<'_>> {
    let Some(workload) = &snapshot.workload else {
        return Vec::new();
    };
    let mut queries: Vec<&QueryStat> = workload
        .queries
        .iter()
        .filter(|q| !is_monitoring_query(q) && heavy_share(snapshot, q) >= HEAVY_SHARE)
        .collect();
    queries.sort_by(|a, b| heavy_share(snapshot, b).total_cmp(&heavy_share(snapshot, a)));
    // A statement whose cost is behind it (it stopped running, or ran once) never takes the place of one that runs:
    // the six are statements that run, and history follows them.
    let (history, running): (Vec<&QueryStat>, Vec<&QueryStat>) =
        queries.into_iter().partition(|q| crate::query_is_history(snapshot, q));
    running
        .into_iter()
        .take(HEAVY_QUERIES)
        .chain(history.into_iter().take(HEAVY_QUERIES))
        .map(|query| HeavyQuery { query, case: decide(snapshot, query) })
        .collect()
}

/// The heavy statements that get a finding of their own, of those that run (and as many of those whose cost is behind
/// them).
const HEAVY_QUERIES: usize = 6;

/// What a heavy statement's finding concludes ([`decide`]), before a word of it is written ([`render`]): what explains
/// its time, how it is graded, the advice the evidence permits, what a later scan shows of it, and the step it is about.
pub struct HeavyCase<'a> {
    /// Weighed, rated and graded on what Culprit's scans measured of it where they did (`query_impact`), the same
    /// figures the title, the summary and every list of statements state.
    impact: QueryImpact,
    severity: Severity,
    /// Its score held to a low finding's: a deliberate pause holds a connection, not the server's resources.
    ranked_low: bool,
    confidence: Confidence,
    actionability: culprit_core::Actionability,
    kind: StatementKind,
    /// It stopped running: told in the past.
    stopped: bool,
    /// What Culprit's own scans measured of it, and that measure again where its entry is recent ([`recent_entry`]).
    scans: Option<culprit_core::ScanSpan>,
    recent: Option<culprit_core::ScanSpan>,
    title: TitleForm,
    /// What explains its time, said after its share.
    lead: Lead<'a>,
    /// The wait that takes most of its time, where the time is not idle.
    wait: Option<DominantWait<'a>>,
    /// The triggers a write fires on its table, whose work runs inside its time: (table, event, names).
    triggers: Option<(&'a str, TriggerEvent, Vec<String>)>,
    /// It is optimizer statistics maintenance, not an update of application rows.
    statistics_maintenance: bool,
    remedy: Remedy<'a>,
    verify: Verify,
    /// The plan step the finding points at.
    plan_node: Option<u32>,
}

/// What a heavy statement's title says of it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TitleForm {
    /// Its share of database time, and, where its time is idle ([`Lead::Idle`]), what it does instead.
    Share,
    /// A read whose runs return less than a row: the pages (rows examined, MySQL) a run reads to return that.
    ReadsForNothing { reads: f64, returns: Returns },
    /// A read that reads at least [`MANY_READS_PER_ROW`] for every row it returns: that many.
    ManyReadsPerRow(f64),
}

/// What a read's runs return, where it is less than a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Returns {
    Nothing,
    LessThanARow,
}

impl Returns {
    /// Its case in a message's `{returns, select, ..}`.
    fn select(self) -> &'static str {
        match self {
            Returns::Nothing => "nothing",
            Returns::LessThanARow => "less_than_a_row",
        }
    }
}

/// The event a write's triggers fire on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TriggerEvent {
    Insert,
    Update,
    Delete,
    /// A MERGE or REPLACE: any of them.
    Write,
}

impl TriggerEvent {
    /// Its case in a message's `{event, select, ..}`.
    fn select(self) -> &'static str {
        match self {
            TriggerEvent::Insert => "insert",
            TriggerEvent::Update => "update",
            TriggerEvent::Delete => "delete",
            TriggerEvent::Write => "write",
        }
    }
}

/// What explains a heavy statement's time, the first of these that applies.
enum Lead<'a> {
    /// Its time isn't work: it sleeps, or waits.
    Idle(Idle),
    /// The step of its plan that does the work, with the share of the plan's estimated cost on it where the plan can
    /// state one: beside a step it puts no cost on, a scan would read as "100%".
    Step { plan: &'a Plan, node: &'a PlanNode, share: Option<f64> },
    /// A seek's residual filter may explain the reads, but only an actual plan can establish its row counts.
    Lookups(ResidualRead<'a>),
    /// A write whose plan costs next to nothing whatever its runs cost: the time is the write itself.
    CheapWrite,
    /// A plan that does no real work (a point lookup), run cheaply as measured: its time is the call itself, so no
    /// step of it is the lead. Where its time waits for storage, with the pages a run reads (`storage_reads`): the few
    /// it reads come from disk, so it isn't how often it runs alone.
    CheapPlan { storage_reads: Option<f64> },
    /// Costed under the bar every step's share needs, yet not cheap because a step reads a whole table: no share of
    /// the time is put on that step, but it is the one the fix is about.
    WholeScan { plan: &'a Plan, node: &'a PlanNode },
    /// Nothing the plan or the statement's waits show.
    Unexplained,
}

/// How the verify step checks the change.
enum Verify {
    /// By what a later scan shows ([`verify_line`]).
    Scan(Expect),
    /// The rewrite's own comparison of the original and the rewritten query, quoted from its observation.
    Quoted(String),
    /// By later statistics refreshes under comparable load.
    StatisticsRefreshes,
}

/// What a heavy statement's finding concludes, from the evidence alone: nothing here is worded.
fn decide<'a>(snapshot: &'a Snapshot, q: &'a QueryStat) -> HeavyCase<'a> {
    let impact = query_impact(snapshot, &[q]).expect("workload present");
    let scans = snapshot.workload.as_ref().and_then(|w| w.scans_of(q));
    // Runs a day at the pace it ran: over Culprit's scans where they measured it, a burst's own total, never
    // extrapolated to a day.
    let per_day_runs = scans.map(|s| s.per_day(s.runs)).or_else(|| rate_per_day_of(snapshot, &[q], q.executions));
    let kind = kind_of(q);
    // Rows returned measure waste only for a read that returns what it finds: an aggregate returns one row per group
    // whatever it reads, and a write returns none.
    let reads_per_row = match (q.reads_per_execution(), q.rows_per_execution()) {
        (Some(reads), Some(rows)) if kind == StatementKind::Read => Some(reads / rows.max(1.0)),
        _ => None,
    };
    // A read whose runs return less than a row: per row, its reads would be counted against a row it rarely returns,
    // so they're said per run, once they are more than a lookup that finds nothing reads.
    let reads_for_nothing = match (q.reads_per_execution(), q.rows_per_execution()) {
        (Some(reads), Some(rows)) if kind == StatementKind::Read && rows < 1.0 && reads >= NOTHING_READS => {
            Some((reads, if rows <= 0.0 { Returns::Nothing } else { Returns::LessThanARow }))
        }
        _ => None,
    };
    let idle = idle_time(snapshot, q);
    let hot =
        q.plan.as_ref().and_then(|p| Some((p, culprit_core::insights::working_step(p)?))).filter(|_| idle.is_none());
    // A plan that does no real work (a point lookup), run cheaply as measured.
    let cheap_plan = q.plan.as_ref().is_some_and(|p| culprit_core::insights::runs_cheaply(p, q));
    // A residual filter may explain the reads, but only an actual plan can establish its row counts.
    let lookups = reads_for_nothing
        .filter(|_| hot.is_none() && idle.is_none())
        .and_then(|(reads, returns)| residual_read(q.plan.as_ref()?, reads, returns));
    // Costed under the bar every step's share needs, yet not cheap because a step reads a whole table.
    let whole_scan = q
        .plan
        .as_ref()
        .filter(|_| hot.is_none() && idle.is_none() && lookups.is_none() && !cheap_plan)
        .and_then(|p| Some((p, whole_table_scan(p)?)));
    let wait = dominant_wait(q).filter(|_| idle.is_none());
    let title = match (&idle, reads_per_row, reads_for_nothing) {
        (Some(_), _, _) => TitleForm::Share,
        (None, _, Some((reads, returns))) => TitleForm::ReadsForNothing { reads, returns },
        (None, Some(r), _) if r >= MANY_READS_PER_ROW => TitleForm::ManyReadsPerRow(r),
        _ => TitleForm::Share,
    };
    let mut severity = severity_for_impact(snapshot, &[q], &impact);
    let mut ranked_low = false;
    let mut confidence = Confidence::High;
    let mut plan_node = hot.map(|(_, n)| n.id);
    let (mut remedy, mut verify) = match &idle {
        Some(idle) => {
            if idle.kind == IdleKind::Sleep || idle.kind == IdleKind::AppLock {
                // A deliberate pause holds a connection, not the server's resources: listed, ranked low.
                severity = Severity::Low;
                ranked_low = true;
            }
            confidence = idle.confidence;
            (Remedy::Idle(idle.kind), Verify::Scan(idle.verify()))
        }
        None => {
            // The next steps tied to one operator: measure a seek's residual filter, or inspect a whole-table read.
            let step = match (&lookups, whole_scan) {
                (Some(lookups), _) => Some((
                    lookups.check.id,
                    Remedy::Measure { schema: lookups.schema.clone(), table: lookups.table.clone() },
                )),
                (None, Some((plan, node))) => {
                    scan_step(snapshot, plan, node, q, kind).map(|step| (node.id, Remedy::ScanStep(step)))
                }
                _ => None,
            };
            let (remedy, lever) = match step {
                Some((node, remedy)) => {
                    plan_node = Some(node);
                    (remedy, Shows::CheaperRuns)
                }
                None => heavy_remedy(snapshot, hot, per_day_runs, q, kind, cheap_plan),
            };
            let expect = match (&lookups, &wait) {
                (Some(_), _) => Expect::Measure,
                // A read that waits for storage: what the change shortens is the wait, not the pages a run reads.
                (None, Some(wait))
                    if wait.on == WaitOn::Storage && matches!(kind, StatementKind::Read | StatementKind::Aggregate) =>
                {
                    Expect::Storage(wait.share)
                }
                _ if culprit_core::sql::is_session_statement(&q.text) => Expect::Session,
                // A fix that cuts runs leaves what one run reads as it was (or more, once the cheap runs are skipped
                // or a batch carries more rows): what moves is how often it runs, and its time with it.
                _ if super::chatty_query::counter_advice(snapshot, q).is_some() => Expect::Counter,
                _ if lever == Shows::FewerRuns => Expect::FewerRuns,
                _ => Expect::CheaperRuns,
            };
            (remedy, Verify::Scan(expect))
        }
    };
    let statistics_maintenance = statistics_maintenance_table(&q.text).is_some();
    if statistics_maintenance {
        (remedy, verify) = (Remedy::StatisticsMaintenance, Verify::StatisticsRefreshes);
    }
    let mut actionability = culprit_core::Actionability::default();
    if let Some(rewrite) = q.plan.as_ref().and_then(|p| late_filter_rewrite(snapshot, q, p)) {
        plan_node = rewrite.nodes.first().copied();
        verify = Verify::Quoted(rewrite.verify.clone());
        remedy = Remedy::Rewrite(rewrite);
        if !index_work_is_material(snapshot, &[q]) {
            actionability = culprit_core::Actionability::Information;
        }
    }
    let lead = match (idle, hot, lookups) {
        (Some(idle), _, _) => Lead::Idle(idle),
        (None, Some((plan, node)), _) => Lead::Step { plan, node, share: plan.step_share(node) },
        (None, None, Some(lookups)) => Lead::Lookups(lookups),
        _ if cheap_plan && kind == StatementKind::Write => Lead::CheapWrite,
        _ if cheap_plan => Lead::CheapPlan {
            storage_reads: wait.as_ref().filter(|w| w.on == WaitOn::Storage).and(q.reads_per_execution()),
        },
        _ => match whole_scan {
            Some((plan, node)) => Lead::WholeScan { plan, node },
            None => Lead::Unexplained,
        },
    };
    HeavyCase {
        impact,
        severity,
        ranked_low,
        confidence,
        actionability,
        kind,
        stopped: crate::query_stopped(snapshot, q).is_some(),
        scans,
        recent: recent_entry(snapshot, q),
        title,
        lead,
        wait,
        triggers: (kind == StatementKind::Write).then(|| target_table_triggers(snapshot, q)).flatten(),
        statistics_maintenance,
        remedy,
        verify,
        plan_node,
    }
}

/// A heavy statement's finding, worded from what [`decide`] concluded. Only phrasing is chosen here: the tense of a
/// statement that stopped, what the engine counts (MySQL's rows examined, the others' pages), the source's names.
fn render(snapshot: &Snapshot, q: &QueryStat, case: &HeavyCase) -> Finding {
    // The statement leads its title: "An update of products".
    let handle = capitalize(&query_handle(q));
    let time = time_noun(snapshot);
    let mut f = finding("heavy-query", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    apply_impact(snapshot, &mut f, &case.impact);
    f.severity = case.severity;
    if case.ranked_low {
        f.impact.score = f.impact.score.min(base_score(Severity::Low));
    }
    f.confidence = case.confidence;
    f.actionability = case.actionability;
    let share = format::percent(case.impact.share);
    // MySQL counts the rows a statement examined, the other engines the pages it read.
    let mysql = snapshot.server.engine == Engine::Mysql;
    // A statement that stopped running is told in the past: it took its share until it stopped, whatever it spent the
    // time on.
    let tense = if case.stopped { "past" } else { "present" };
    f.title = match case.title {
        TitleForm::Share => match &case.lead {
            Lead::Idle(idle) => t!(
                "check.heavy_query.title.share_idle",
                handle = &handle,
                tense = tense,
                share = &share,
                time = &time,
                idle = idle.title_tail()
            ),
            _ => t!("check.heavy_query.title.share", handle = &handle, tense = tense, share = &share, time = &time),
        },
        TitleForm::ReadsForNothing { reads, returns } => {
            let (reads, returns) = (format::count(reads), returns.select());
            if mysql {
                t!(
                    "check.heavy_query.title.reads_for_nothing_rows_examined",
                    handle = &handle,
                    tense = tense,
                    reads = reads,
                    returns = returns
                )
            } else {
                t!(
                    "check.heavy_query.title.reads_for_nothing_pages",
                    handle = &handle,
                    tense = tense,
                    reads = reads,
                    returns = returns
                )
            }
        }
        TitleForm::ManyReadsPerRow(r) => {
            let reads = format::count(r);
            if mysql {
                t!(
                    "check.heavy_query.title.reads_per_row_rows_examined",
                    handle = &handle,
                    tense = tense,
                    reads = reads
                )
            } else {
                t!("check.heavy_query.title.reads_per_row_pages", handle = &handle, tense = tense, reads = reads)
            }
        }
    };
    let workload = snapshot.workload.as_ref();
    // With time the source couldn't attribute, the share is of what it could.
    let of = match workload.filter(|w| w.untracked_time_ms.is_some()) {
        Some(w) => t!("check.heavy_query.summary.of_attributed", time = time, source = &w.source),
        None => t!("check.heavy_query.summary.of_all", time = time),
    };
    let window = workload.map(window_phrase).unwrap_or_default();
    let mut summary = vec![match &case.recent {
        Some(s) => t!(
            "check.heavy_query.summary.recent",
            source = workload.map_or_else(|| t!("check.heavy_query.the_statistics"), |w| w.source.clone()),
            runs = format::count(s.runs),
            executions = format::count(q.executions),
            length = s.length(),
            per_day = format::count(s.per_day(s.runs)),
            mean = format::duration(s.time_ms / s.runs.max(1.0)),
            share = &share,
            window = &window,
            totals_share = format::percent(workload.map_or(0.0, |w| w.share_of(q.total_time_ms, q)))
        ),
        // What runs now, where the source's totals go back further than Culprit's scans: their figures, with the
        // totals' beside them where those differ twofold.
        None => scans_sentence(snapshot, q, &of).unwrap_or_else(|| {
            let (runs, when) = statement_runs(snapshot, q);
            let mean = format::duration(q.mean_time_ms);
            match q.few_runs_took().filter(|_| q.executions > 1.0) {
                // Two runs far apart: what each took, never an average that hides it.
                Some(took) => t!(
                    "check.heavy_query.summary.ran_taking",
                    runs = runs,
                    took = took,
                    when = when,
                    share = &share,
                    of = &of,
                    window = &window
                ),
                None if q.executions > 1.0 => t!(
                    "check.heavy_query.summary.ran_on_average",
                    runs = runs,
                    mean = mean,
                    when = when,
                    share = &share,
                    of = &of,
                    window = &window
                ),
                None => t!(
                    "check.heavy_query.summary.ran_at",
                    runs = runs,
                    mean = mean,
                    when = when,
                    share = &share,
                    of = &of,
                    window = &window
                ),
            }
        }),
    }];
    match &case.lead {
        Lead::Idle(idle) => summary.push(idle.evidence(snapshot, q)),
        Lead::Step { plan, node, share } => {
            // MySQL counts the rows a statement examined: what the step reads, measured, when it is the plan's only
            // table.
            let examined = q.reads_per_execution().filter(|_| mysql);
            let step = describe_node(plan, node, examined);
            summary.push(match share {
                Some(s) => t!("check.heavy_query.summary.step_share", share = format::percent(*s), step = step),
                None => uncosted_step(snapshot, plan, &step),
            });
        }
        Lead::Lookups(lookups) => summary.push(lookups.evidence()),
        Lead::CheapWrite => summary.push(write_cost(snapshot, q)),
        Lead::CheapPlan { storage_reads: Some(reads) } => {
            let reads = format::count(reads.round().max(1.0));
            summary.push(if mysql {
                t!("check.heavy_query.summary.cheap_plan_reads_rows_examined", reads = reads)
            } else {
                t!("check.heavy_query.summary.cheap_plan_reads_pages", reads = reads)
            });
        }
        Lead::CheapPlan { storage_reads: None } => summary.push(t!("check.heavy_query.summary.cheap_plan")),
        Lead::WholeScan { plan, node } => {
            // No share of the cost is put on the scan under the bar, but that every run reads its table is the plan's
            // own.
            let step = describe_node(plan, node, q.reads_per_execution().filter(|_| mysql));
            summary.push(t!("check.heavy_query.summary.whole_scan", step = step));
        }
        Lead::Unexplained => {}
    }
    if let Some(wait) = &case.wait {
        summary.push(wait.sentence(snapshot, q));
    }
    if let Some((table, event, names)) = &case.triggers {
        summary.push(t!(
            "check.heavy_query.summary.triggers",
            table = *table,
            count = names.len(),
            event = event.select(),
            names = names.join(", ")
        ));
    }
    if case.statistics_maintenance {
        summary.push(t!("check.heavy_query.summary.statistics_maintenance"));
    }
    f.summary = summary.join(" ");
    // A figure kept only in the source's totals, where the summary leads with Culprit's scans: said over them.
    let over = |label: String, span: &str| t!("common.metric.over", label = label, span = span);
    match (workload, &case.scans) {
        // What the scans measured, and the totals since the source began counting where they differ twofold (never
        // for a recent entry, whose totals are those runs diluted by the time before it began).
        (Some(w), Some(s)) => {
            let totals = w.window_phrase();
            let beside = !s.is_recent(q);
            f.metrics.push(Metric::new(t!("common.metric.share_of_db_time_over_scans"), s.share(), Unit::Ratio));
            if beside && s.share_differs(w, q) {
                let label = over(share_metric_label(snapshot), &totals);
                f.metrics.push(Metric::new(label, w.share_of(q.total_time_ms, q), Unit::Ratio));
            }
            // Runs that came in bursts are counted, never a rate a day.
            if s.per_day(s.runs) >= 1.0 && !w.cadence(q).is_burst() {
                let label = t!("common.metric.runs_per_day_over_scans");
                f.metrics.push(Metric::new(label, s.per_day(s.runs), Unit::Count));
            } else {
                f.metrics.push(Metric::new(t!("common.metric.runs_over_scans"), s.runs, Unit::Count));
            }
            if beside && s.runs_differ(w, q) && !w.cadence(q).is_burst() {
                let label = over(t!("common.metric.runs_per_day"), &totals);
                f.metrics.push(Metric::new(label, w.per_day(q.executions), Unit::Count));
            }
            f.metrics.extend(average_metrics(snapshot, q));
        }
        _ => {
            f.metrics.push(Metric::new(share_metric_label(snapshot), case.impact.share, Unit::Ratio));
            match workload.and_then(|w| Some((w.runs_per_day_of(q), w.per_day_measured(q.executions)?))) {
                Some((Some(runs), _)) => {
                    f.metrics.push(Metric::new(t!("common.metric.runs_per_day"), runs, Unit::Count))
                }
                // A day or more of window, but not runs that came a day at a time: what was counted.
                Some((None, _)) => f.metrics.push(Metric::said(key!("common.metric.runs"), q.executions, Unit::Count)),
                None => {}
            }
            f.metrics.extend(average_metrics(snapshot, q));
        }
    }
    let basis = crate::support::totals_basis(snapshot, q);
    if let Some(r) = q.reads_per_execution() {
        // MySQL counts rows examined, the other engines pages.
        let (label, unit) = if mysql {
            (t!("common.metric.rows_examined_per_run"), Unit::Rows)
        } else {
            (t!("common.metric.pages_read_per_run"), Unit::Pages)
        };
        let label = match &basis {
            Some(basis) => over(label, basis),
            None => label,
        };
        f.metrics.push(Metric::new(label, r, unit));
    }
    if let (Some(r), Some(labels)) = (q.rows_per_execution(), rows_labels(snapshot.server.engine, case.kind)) {
        let mut metric = rows_metric(labels, r, q.executions);
        if let Some(basis) = &basis {
            metric.label = over(metric.label, basis);
        }
        f.metrics.push(metric);
    }
    if let Some(cpu) = q.cpu_time_ms.filter(|_| q.total_time_ms > 0.0) {
        f.metrics.push(Metric::new(t!("common.metric.cpu_share"), (cpu / q.total_time_ms).min(1.0), Unit::Ratio));
    }
    if let Some((_, _, names)) = &case.triggers {
        f.metrics.push(Metric::text(t!("check.heavy_query.metric.triggers"), names.join(", ")));
    }
    f.fix = Some(case.remedy.fix(snapshot, q));
    f.verify = vec![match &case.verify {
        Verify::Scan(expect) => verify_line(snapshot, q, *expect),
        Verify::Quoted(line) => line.clone(),
        Verify::StatisticsRefreshes => t!("check.heavy_query.verify.statistics_refreshes"),
    }];
    f.queries = vec![QueryRef { id: q.id.clone(), plan_node: case.plan_node, note: None }];
    f
}

/// What a change shows in a later scan, for the verify step ([`verify_line`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Expect {
    /// The cause is not known yet: get operator measurements before prescribing a change.
    Measure,
    /// Less of its time waiting for storage, which is this share of it now (Query Store sums a parallel plan's waits
    /// over its threads, which can pass the whole).
    Storage(f64),
    /// A fix that makes each run cheaper: its reads and time per run drop.
    CheaperRuns,
    /// A fix that cuts runs (a cache, a skipped or batched call, a rarer poll): fewer runs and less time in all, while
    /// what one run reads and takes stays, or rises.
    FewerRuns,
    /// A counter's calls may be required to allocate numbers and return state atomically.
    Counter,
    /// A session statement: evaluate avoidable overhead without removing required session or transaction work.
    Session,
    /// Its average run, toward what its reads take (a wait taken away).
    Average,
    /// Its time, or that it stops running (a pause or an application lock taken away).
    TimeOrStops,
}

/// How a later scan shows the change. A windowed source (Query Store, a log) covers the runs since, so its share,
/// reads and averages move; on a source of running totals two of Culprit's scans show it, in What changed, which
/// carries runs and time and no reads ([`verify_between_scans`]).
fn verify_line(snapshot: &Snapshot, q: &QueryStat, expect: Expect) -> String {
    // MySQL counts the rows a statement examined, the other engines the pages it read.
    let mysql = snapshot.server.engine == Engine::Mysql;
    if matches!(expect, Expect::Measure) {
        return if mysql {
            t!("check.heavy_query.verify.measure_rows_examined")
        } else {
            t!("check.heavy_query.verify.measure_pages")
        };
    }
    // The storage wait's share now: "most" where a parallel plan's waits pass the whole.
    let storage_now = |share: f64| (if share >= 1.0 { "most" } else { "share" }, format::percent(share));
    let between = match expect {
        Expect::Measure => unreachable!(),
        Expect::Storage(share) => {
            let (part, share) = storage_now(share);
            t!("check.heavy_query.verify.between.storage", part = part, share = share)
        }
        Expect::CheaperRuns => Shows::CheaperRuns.between(),
        Expect::FewerRuns => Shows::FewerRuns.between(),
        Expect::Counter => t!("check.heavy_query.verify.between.counter"),
        Expect::Session => t!("check.heavy_query.verify.between.session"),
        Expect::Average => t!("check.heavy_query.verify.between.average"),
        Expect::TimeOrStops => t!("check.heavy_query.verify.between.time_or_stops"),
    };
    if let Some(line) = verify_between_scans(snapshot, q, &between) {
        return line;
    }
    let time = time_noun(snapshot);
    match expect {
        Expect::Measure => unreachable!(),
        Expect::Storage(share) => {
            let (part, share) = storage_now(share);
            t!("check.heavy_query.verify.storage", part = part, share = share)
        }
        Expect::CheaperRuns if mysql => t!("check.heavy_query.verify.cheaper_runs_rows_examined", time = time),
        Expect::CheaperRuns => t!("check.heavy_query.verify.cheaper_runs_pages", time = time),
        Expect::FewerRuns => t!("check.heavy_query.verify.fewer_runs", time = time),
        Expect::Counter => t!("check.heavy_query.verify.counter"),
        Expect::Session => t!("check.heavy_query.verify.session"),
        Expect::Average => t!("check.heavy_query.verify.average"),
        Expect::TimeOrStops => t!("check.heavy_query.verify.time_or_stops"),
    }
}

/// "Rows returned per run" as a number, or, for so few that a number per run would read as none ("0.0" for 1 row in
/// 6.5K runs), as what was counted: "Rows returned", "1 row in 6.5K runs". `labels` names the rows a run and in all
/// ([`rows_labels`]).
fn rows_metric((per_run_label, total_label): (String, String), per_run: f64, runs: f64) -> Metric {
    let rows = per_run * runs;
    if per_run > 0.0 && per_run < 0.05 && runs > 0.0 {
        let rows = rows.round().max(1.0);
        let counted = t!(
            "check.heavy_query.metric.rows_in_runs",
            rows = format::count(rows),
            count = rows,
            runs = format::count(runs)
        );
        return Metric::text(total_label, counted);
    }
    if per_run > 0.0 && per_run < 1.0 {
        return Metric::text(per_run_label, rows_a_run(per_run));
    }
    Metric::new(per_run_label, per_run, Unit::Rows)
}

/// Reads per row returned from which a read's title says so, and an aggregate's result is worth keeping up to date
/// instead of recomputing: thousands of pages (rows examined, MySQL) folded into each row it returns.
pub(super) const MANY_READS_PER_ROW: f64 = 1000.0;

/// The costliest step of a plan that puts no cost on another step doing work ([`Plan::uncosted_steps`]: MySQL's
/// filesort and temporary tables), or whose rows rest on a guess, in plain words: " Its only step with a cost is a table
/// scan of shop.orders that examines 500K rows a run (MySQL puts no cost on the sort)." No share is said:
/// [`Plan::step_share`] gives none.
fn uncosted_step(snapshot: &Snapshot, plan: &Plan, step: &str) -> String {
    let mut steps: Vec<Uncosted> = Vec::new();
    for n in plan.uncosted_steps() {
        let temporary = [Some(n.op.as_str()), n.detail.as_deref()]
            .into_iter()
            .flatten()
            .any(|t| t.to_ascii_lowercase().contains("temporary"));
        let what = match n.kind {
            _ if temporary => Uncosted::TemporaryTable,
            NodeKind::Sort => Uncosted::Sort,
            NodeKind::Aggregate => Uncosted::Grouping,
            _ => Uncosted::Spool,
        };
        if !steps.contains(&what) {
            steps.push(what);
        }
    }
    if steps.is_empty() {
        return t!("check.heavy_query.summary.step_costed_highest", step = step);
    }
    let uncosted: Vec<String> = steps
        .into_iter()
        .map(|what| match what {
            Uncosted::TemporaryTable => t!("check.heavy_query.uncosted.temporary_table"),
            Uncosted::Sort => t!("check.heavy_query.uncosted.sort"),
            Uncosted::Grouping => t!("check.heavy_query.uncosted.grouping"),
            Uncosted::Spool => t!("check.heavy_query.uncosted.spool"),
        })
        .collect();
    let (engine, uncosted) = (snapshot.server.name(), format::list(&uncosted));
    let costed = plan.nodes().into_iter().filter(|n| n.self_cost > 0.0).count();
    if costed <= 1 {
        t!("check.heavy_query.summary.only_costed_step", step = step, engine = engine, uncosted = uncosted)
    } else {
        t!("check.heavy_query.summary.costliest_costed_step", step = step, engine = engine, uncosted = uncosted)
    }
}

/// A step doing work that its plan puts no cost on ([`Plan::uncosted_steps`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Uncosted {
    TemporaryTable,
    Sort,
    Grouping,
    Spool,
}

/// The costliest step that reads a whole table, in a plan that weighs its steps: what keeps a plan costed under the bar
/// from running cheaply ([`culprit_core::insights::runs_cheaply`]), since a scan costs more as its table grows.
fn whole_table_scan(plan: &Plan) -> Option<&PlanNode> {
    if !plan.weighed() {
        return None;
    }
    // A scan that stops at its first rows (under a TOP, in index order) doesn't read its table whole.
    plan.nodes().into_iter().filter(|n| n.reads_table_whole()).max_by(|a, b| a.self_cost.total_cmp(&b.self_cost))
}

/// What a statement does with the rows it touches, as far as its text says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatementKind {
    /// Returns the rows it finds.
    Read,
    /// Returns one row per group, or one row in all: COUNT, SUM, GROUP BY, DISTINCT.
    Aggregate,
    /// INSERT, UPDATE, DELETE, MERGE: changes rows and returns none.
    Write,
    /// A read that locks the rows it finds until its transaction ends (`SELECT … FOR UPDATE`, `FOR SHARE`, SQL Server's
    /// UPDLOCK): it waits for a transaction holding them, as a write does.
    Locking,
    Other,
}

pub(crate) fn statement_kind(text: &str) -> StatementKind {
    let lower = squeeze(culprit_core::sql::without_parameter_list(text));
    // The statement a WITH list leads to: `WITH b AS (SELECT …) DELETE …` is a delete.
    let body = main_statement(&lower).trim_start_matches(|c: char| c.is_whitespace() || c == '(');
    if ["insert", "update", "delete", "merge", "replace"].iter().any(|v| body.starts_with(v)) {
        return StatementKind::Write;
    }
    if !(body.starts_with("select") || body.starts_with("with")) {
        return StatementKind::Other;
    }
    if locking_clause(&lower).is_some() {
        return StatementKind::Locking;
    }
    let aggregate = ["count(", "sum(", "avg(", "min(", "max(", "string_agg(", "array_agg(", "group_concat("]
        .iter()
        .any(|f| calls(&lower, f))
        || lower.contains(" group by ")
        || lower.contains("select distinct ");
    if aggregate { StatementKind::Aggregate } else { StatementKind::Read }
}

/// [`statement_kind`] for a statement, with what its plan says: a PostgreSQL plan that locks the rows it returns
/// (LockRows) is a locking read whatever the text shows of it.
pub(crate) fn kind_of(q: &QueryStat) -> StatementKind {
    let kind = statement_kind(&q.text);
    let locks_rows = q.plan.as_ref().is_some_and(|p| p.nodes().iter().any(|n| n.op.eq_ignore_ascii_case("LockRows")));
    if matches!(kind, StatementKind::Read | StatementKind::Aggregate) && locks_rows {
        StatementKind::Locking
    } else {
        kind
    }
}

/// The clause by which a read locks the rows it finds, as written: `FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`,
/// `FOR KEY SHARE`, MySQL's `LOCK IN SHARE MODE`, SQL Server's UPDLOCK, XLOCK and HOLDLOCK hints. From squeezed text.
pub(super) fn locking_clause(squeezed: &str) -> Option<&'static str> {
    const CLAUSES: [(&str, &str); 8] = [
        ("for update", "FOR UPDATE"),
        ("for no key update", "FOR NO KEY UPDATE"),
        ("for share", "FOR SHARE"),
        ("for key share", "FOR KEY SHARE"),
        ("lock in share mode", "LOCK IN SHARE MODE"),
        ("updlock", "UPDLOCK"),
        ("xlock", "XLOCK"),
        ("holdlock", "HOLDLOCK"),
    ];
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    CLAUSES.iter().find_map(|(clause, name)| {
        squeezed
            .match_indices(clause)
            .any(|(at, _)| {
                !word(squeezed[..at].chars().next_back()) && !word(squeezed[at + clause.len()..].chars().next())
            })
            .then_some(*name)
    })
}

/// Whether squeezed text calls `function` (written with its opening parenthesis), as a whole name.
pub(super) fn calls(squeezed: &str, function: &str) -> bool {
    squeezed.match_indices(function).any(|(at, _)| {
        squeezed[..at].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
    })
}

/// What a statement's rows are called, a run and in all: "Rows returned per run" and "Rows returned", or what the
/// engine's row count means for a write. MySQL counts no rows sent for a write.
pub(super) fn rows_labels(engine: Engine, kind: StatementKind) -> Option<(String, String)> {
    if kind == StatementKind::Write {
        // The engine's label is a run's ("Rows changed per run"); in all, it is the same without "per run".
        let label = engine.spec().statement_metrics().write_rows_label?;
        let total = label.trim_end_matches(" per run").to_string();
        Some((label.into_owned(), total))
    } else {
        Some((t!("common.metric.rows_returned_per_run"), t!("common.metric.rows_returned")))
    }
}

/// Whether the workload adds up only the runs the slow query log kept (those over long_query_time), so a share is of
/// the logged slow runs' time, not of all database time.
pub(crate) fn slow_log_only(snapshot: &Snapshot) -> bool {
    snapshot.workload.as_ref().is_some_and(|w| {
        // The collector says so; scans before it did are read from the setting.
        w.min_logged_ms.is_some()
            || (w.kind() == Some(culprit_core::WorkloadKind::SlowLog) && logs_slow_runs_only(snapshot))
    })
}

/// Whether the slow query log keeps only runs over long_query_time: unless it is 0, which logs every run.
pub(crate) fn logs_slow_runs_only(snapshot: &Snapshot) -> bool {
    !snapshot
        .setting("long_query_time")
        .and_then(|s| s.value.trim().parse::<f64>().ok())
        .is_some_and(|seconds| seconds <= 0.0)
}

/// A fix in words only: what the application changes.
pub(super) fn advice(summary: String) -> Fix {
    Fix { summary, sql: None, cost: None }
}

/// Active triggers on a write statement's target table that fire for the statement's DML event (`INSERT`, `UPDATE`,
/// `DELETE`, `MERGE`/`REPLACE`).
fn target_table_triggers<'a>(snapshot: &'a Snapshot, q: &QueryStat) -> Option<(&'a str, TriggerEvent, Vec<String>)> {
    let verb = first_verb(main_statement(&q.text)).map(|(_, v)| v)?;
    let (wanted, event): (&[char], TriggerEvent) = match verb {
        "insert" => (&['I'], TriggerEvent::Insert),
        "update" => (&['U'], TriggerEvent::Update),
        "delete" => (&['D'], TriggerEvent::Delete),
        "merge" | "replace" => (&['I', 'U', 'D'], TriggerEvent::Write),
        _ => return None,
    };
    let target = culprit_core::sql::tables_in(&q.text, snapshot.server.engine).into_iter().next()?;
    let leaf = target.rsplit('.').next().unwrap_or(&target);
    let table = snapshot.tables.iter().find(|t| t.name.eq_ignore_ascii_case(leaf))?;
    let storage = table.storage.as_ref()?;
    let mut names: Vec<String> = storage
        .triggers()
        .into_iter()
        .filter(|(_, events)| events.is_empty() || wanted.iter().any(|w| events.contains(*w)))
        .map(|(name, _)| name.to_string())
        .collect();
    names.sort();
    names.dedup();
    (!names.is_empty()).then_some((table.name.as_str(), event, names))
}
