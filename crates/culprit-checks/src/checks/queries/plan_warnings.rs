//! The `plan-warnings` check: Implicit conversions, spills, missing join predicates and missing statistics in the plans of heavy queries.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::insights::{PlanInsight, StaleIndexStatistics};
use culprit_core::t;

pub fn plan_warnings_info() -> CheckInfo {
    CheckInfo {
        id: "plan-warnings".into(),
        title: t!("check.plan_warnings.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql],
        requires: vec![Capability::QueryPlans],
        description: t!("check.plan_warnings.info.description"),
    }
}

check!(
    PlanWarningsCheck: plan_warnings_info,
    decide = plan_warning_cases,
    render = plan_warning_finding,
    case<'s> = PlanWarningCase<'s>,
    evidence = &[Evidence::Query(QueryMeasure::CapturedPlans)],
);

/// A warning in the plans of one or more statements, and what it establishes: one cause (a type mismatch on one
/// column) is one case, however many statements' plans show it.
pub struct PlanWarningCase<'s> {
    /// The first statement whose plan warned.
    query: &'s QueryStat,
    impact: QueryImpact,
    /// What the plan warned of, as the first statement's plan shows it.
    warned: Warned,
    /// The type mismatch to describe, where several statements show it: SQL Server's verdict that it stops a seek, from
    /// whichever statement's plan says so.
    said: Option<Conversion>,
    severity: Severity,
    confidence: Confidence,
    /// A warning about a plan that runs cheaply, or one a saved plan alone can't establish: information.
    informational: bool,
    /// Every statement whose plan shows it, with the step it warned on.
    steps: Vec<(&'s QueryStat, Option<u32>)>,
    /// What several statements that share it take together, and whether they all stopped.
    shared: Option<Shared>,
}

struct Shared {
    impact: QueryImpact,
    queries: usize,
    stopped: bool,
}

/// What one statement's plan warned of.
enum Warned {
    Conversion(Conversion),
    Spill,
    NoJoinPredicate(JoinCondition),
    MissingStatistics(Box<StatisticsWarning>),
}

/// A compilation-time statistics warning, and what reads it.
struct StatisticsWarning {
    /// The plan's own reading of the warning.
    insight: Option<PlanInsight>,
    /// The index keys it names, where every column it names is one.
    stale: Option<StaleIndexStatistics>,
    /// What the catalog says of those statistics now.
    now: Option<StatisticsNow>,
}

/// A type mismatch: the database converts a column to compare it with a parameter of another type.
#[derive(Clone)]
struct Conversion {
    /// The column converted, and the type it is converted to, where the plan says.
    column: Option<String>,
    target: Option<String>,
    /// SQL Server says the conversion stops a seek ("Seek Plan"), not only that it affects estimates.
    blocks_seek: bool,
    /// The conversion as the plan writes it.
    expression: Option<String>,
    /// The parameters the application declares.
    parameters: Option<String>,
}

/// What the catalog says now of the statistics a plan warned held nothing usable.
struct StatisticsNow {
    indexes: Vec<IndexStatistics>,
    /// The table's rows when the plan was compiled, and now.
    table_then: Option<f64>,
    table_now: Option<f64>,
}

struct IndexStatistics {
    index: String,
    updated: String,
    /// The table's rows when they were updated, and the changes counted since.
    rows: Option<f64>,
    changes: Option<f64>,
}

/// Every plan warning worth a finding, one case per cause.
fn plan_warning_cases(snapshot: &Snapshot) -> Vec<PlanWarningCase<'_>> {
    let mut out = Vec::new();
    for q in app_queries(snapshot) {
        let Some(plan) = &q.plan else { continue };
        let Some(impact) = query_impact(snapshot, &[q]) else {
            continue;
        };
        if impact.share < 0.005 {
            continue;
        }
        for warning in [
            PlanWarning::ImplicitConversion,
            PlanWarning::Spill,
            PlanWarning::NoJoinPredicate,
            PlanWarning::MissingStatistics,
        ] {
            let nodes: Vec<&PlanNode> = plan.nodes().into_iter().filter(|n| n.warnings.contains(&warning)).collect();
            if nodes.is_empty() && !plan.warnings.contains(&warning) {
                continue;
            }
            if let Some(case) = plan_warning_case(snapshot, q, &impact, warning, nodes.first().copied()) {
                out.push(case);
            }
        }
    }
    merge_same_cause(snapshot, out)
}

impl PlanWarningCase<'_> {
    /// The finding's id: a type mismatch's column, so every statement that shows it is one finding.
    fn id(&self) -> String {
        match &self.warned {
            Warned::Conversion(Conversion { column: Some(c), .. }) => format!("plan-warnings:implicit-conversion:{c}"),
            _ => format!("plan-warnings:{}:{}", self.query.id, warning_key(self.warning())),
        }
    }

    fn warning(&self) -> PlanWarning {
        match self.warned {
            Warned::Conversion(_) => PlanWarning::ImplicitConversion,
            Warned::Spill => PlanWarning::Spill,
            Warned::NoJoinPredicate(_) => PlanWarning::NoJoinPredicate,
            Warned::MissingStatistics(_) => PlanWarning::MissingStatistics,
        }
    }
}

/// One root cause (a type mismatch on one column) is one case, however many queries hit it. It stops them seeking an
/// index only where SQL Server said so of one of them (a "Seek Plan" warning, kept as High confidence); otherwise the
/// conversion slows them, and their share doesn't raise it. Seeking, not using: the plan may still scan the index.
fn merge_same_cause<'s>(snapshot: &'s Snapshot, cases: Vec<PlanWarningCase<'s>>) -> Vec<PlanWarningCase<'s>> {
    let mut merged: Vec<PlanWarningCase> = Vec::new();
    for c in cases {
        match merged.iter_mut().find(|m| m.id() == c.id()) {
            Some(m) => {
                if c.confidence == Confidence::High && m.confidence != Confidence::High {
                    m.confidence = Confidence::High;
                    m.severity = m.severity.max(c.severity);
                    m.said = match &c.warned {
                        Warned::Conversion(conversion) => Some(conversion.clone()),
                        _ => None,
                    };
                }
                for step in c.steps {
                    if !m.steps.iter().any(|(q, _)| q.id == step.0.id) {
                        m.steps.push(step);
                    }
                }
            }
            None => merged.push(c),
        }
    }
    for m in merged.iter_mut().filter(|m| m.steps.len() > 1) {
        let queries: Vec<&QueryStat> =
            m.steps.iter().filter_map(|(r, _)| snapshot.workload.as_ref()?.query(&r.id)).collect();
        let Some(impact) = query_impact(snapshot, &queries) else { continue };
        if m.confidence == Confidence::High {
            m.severity = m.severity.max(severity_for_impact(snapshot, &queries, &impact));
        }
        m.shared = Some(Shared { queries: queries.len(), stopped: crate::all_stopped(snapshot, &queries), impact });
    }
    merged
}

fn plan_warning_case<'s>(
    snapshot: &'s Snapshot,
    q: &'s QueryStat,
    impact: &QueryImpact,
    warning: PlanWarning,
    node: Option<&PlanNode>,
) -> Option<PlanWarningCase<'s>> {
    let mut informational = false;
    let (warned, severity, confidence) = match warning {
        PlanWarning::ImplicitConversion => {
            let mut named: Vec<culprit_core::PlanConversion> =
                node.into_iter().flat_map(|n| n.facts().conversions.clone()).collect();
            named.extend(q.plan.iter().flat_map(|p| p.facts().conversions.clone()));
            if named.is_empty() {
                named.extend(
                    node.into_iter()
                        .flat_map(|n| [n.predicate.as_ref(), n.seek_predicate.as_ref()])
                        .flatten()
                        .filter(|s| s.contains("CONVERT"))
                        .map(|expression| culprit_core::PlanConversion {
                            issue: culprit_core::PlanConversionIssue::Other("Convert".into()),
                            expression: expression.clone(),
                        }),
                );
            }
            let still = |c: &&culprit_core::PlanConversion| {
                q.plan.as_ref().map_or_else(
                    || converts_column(&c.expression),
                    |p| !current_conversions(p, &c.expression).is_empty(),
                )
            };
            let lines: Vec<_> = named.iter().filter(still).collect();
            if !named.is_empty() && lines.is_empty() {
                return None;
            }
            let issue = lines
                .iter()
                .find(|c| c.issue == culprit_core::PlanConversionIssue::SeekPlan)
                .or(lines.first())
                .copied();
            // "Can't seek an index" is SQL Server's verdict (a "Seek Plan" warning), not a reading of the conversion:
            // without it the plan may still seek (a range through the conversion), and the warning is about its
            // estimates. With it, the plan may still scan the index, so the index isn't unused: it can't be searched.
            let blocks_seek = issue.is_some_and(|c| c.issue == culprit_core::PlanConversionIssue::SeekPlan);
            let converted = issue.and_then(|c| current_conversions(q.plan.as_ref()?, &c.expression).into_iter().next());
            let conversion = Conversion {
                column: converted.as_ref().map(|(c, _)| c.clone()),
                target: converted.map(|(_, t)| t),
                blocks_seek,
                expression: issue.map(|c| c.expression.clone()),
                parameters: q.parameters.clone(),
            };
            let severity = if blocks_seek { severity_for_impact(snapshot, &[q], impact) } else { Severity::Low };
            (Warned::Conversion(conversion), severity, if blocks_seek { Confidence::High } else { Confidence::Medium })
        }
        PlanWarning::Spill => {
            // A spill too small to matter stays Low, as the plan's own `spill` insight rates it.
            let severity = if q.plan.as_ref().is_some_and(culprit_core::insights::spills_are_minor) {
                Severity::Low
            } else {
                severity_for_impact(snapshot, &[q], impact)
            };
            (Warned::Spill, severity, Confidence::High)
        }
        PlanWarning::NoJoinPredicate => {
            let join = join_condition(&q.text);
            let severity = match join {
                JoinCondition::Missing => severity_for_impact(snapshot, &[q], impact),
                JoinCondition::Present => {
                    informational = q.plan.as_ref().is_some_and(|p| culprit_core::insights::runs_cheaply(p, q));
                    Severity::Low
                }
                JoinCondition::CrossJoin => Severity::Low,
            };
            (Warned::NoJoinPredicate(join), severity, Confidence::Medium)
        }
        PlanWarning::MissingStatistics => {
            informational = true;
            // The plan viewer and finding use the same compilation-time evidence and diagnostic guidance.
            let insight = q
                .plan
                .as_ref()
                .and_then(|p| culprit_core::insights::insights(p).into_iter().find(|i| i.rule == "missing-statistics"));
            // The catalog is current context; a saved compilation warning cannot establish staleness now.
            let stale = insight.as_ref().and(q.plan.as_ref()).and_then(culprit_core::insights::stale_index_statistics);
            let now = stale.as_ref().and_then(|stale| statistics_now(snapshot, stale));
            (
                Warned::MissingStatistics(Box::new(StatisticsWarning { insight, stale, now })),
                Severity::Low,
                Confidence::Medium,
            )
        }
        _ => return None,
    };
    Some(PlanWarningCase {
        query: q,
        impact: query_impact(snapshot, &[q])?,
        warned,
        said: None,
        severity,
        confidence,
        informational,
        steps: vec![(q, node.map(|n| n.id))],
        shared: None,
    })
}

fn plan_warning_finding(snapshot: &Snapshot, case: PlanWarningCase<'_>) -> Finding {
    let q = case.query;
    let id = case.id();
    let warning = case.warning();
    let mut f = finding(
        "plan-warnings",
        &format!("{}:{}", q.id, warning_key(warning)),
        Category::Queries,
        subject(SubjectKind::Query, &q.id),
    );
    apply_impact(snapshot, &mut f, &case.impact);
    f.queries = case
        .steps
        .iter()
        .map(|(r, step)| QueryRef { id: r.id.clone(), plan_node: *step, note: Some(warning.label().into()) })
        .collect();
    let share_metric = Metric::new(share_metric_label(snapshot), case.impact.share, Unit::Ratio);
    f.metrics.push(share_metric.clone());
    f.metrics.push(Metric::new(t!("common.metric.average_duration"), q.mean_time_ms, Unit::Ms));
    let query = query_handle(q);
    // A statement that stopped running is told in the past.
    let tense = if crate::all_stopped(snapshot, &[q]) { "past" } else { "present" };
    if case.informational {
        f.informational();
    }
    f.severity = case.severity;
    f.confidence = case.confidence;
    match &case.warned {
        Warned::Conversion(conversion) => {
            f.title = match &conversion.column {
                Some(c) if conversion.blocks_seek => {
                    t!("check.plan_warnings.title.conversion_stops_seek", column = c, query = query, tense = tense)
                }
                Some(c) => t!("check.plan_warnings.title.conversion", column = c, query = query, tense = tense),
                None => t!("check.plan_warnings.title.conversion_unknown", query = query, tense = tense),
            };
            f.summary = conversion_summary(case.said.as_ref().unwrap_or(conversion));
            f.fix = Some(Fix {
                summary: mismatch_fix(conversion.column.as_deref(), conversion.target.as_deref()),
                sql: None,
                cost: None,
            });
            if let Some(c) = &conversion.column {
                f.id = id;
                f.subject = subject(SubjectKind::Table, c);
                f.metrics.push(Metric::text(t!("check.plan_warnings.metric.column"), c.clone()));
            }
        }
        Warned::Spill => {
            f.title = t!("check.plan_warnings.title.spill", query = capitalize(&query), tense = tense);
            f.summary = t!("check.plan_warnings.summary.spill");
            f.fix = Some(Fix {
                summary: match snapshot.server.engine {
                    Engine::Mssql => t!("check.plan_warnings.fix.spill.mssql"),
                    Engine::Postgres | Engine::Yugabyte => t!("check.plan_warnings.fix.spill.postgres"),
                    Engine::Sqlite => t!("check.plan_warnings.fix.spill.sqlite"),
                    Engine::Mysql => t!("check.plan_warnings.fix.spill.mysql"),
                },
                sql: None,
                cost: None,
            });
        }
        Warned::NoJoinPredicate(join) => {
            let (title, summary, fix) = match join {
                JoinCondition::Missing => (
                    t!("check.plan_warnings.title.join_missing", query = capitalize(&query), tense = tense),
                    t!("check.plan_warnings.summary.join_missing"),
                    t!("check.plan_warnings.fix.join_missing"),
                ),
                JoinCondition::Present => (
                    t!("check.plan_warnings.title.join_dropped", query = capitalize(&query), tense = tense),
                    t!("check.plan_warnings.summary.join_dropped", engine = snapshot.server.name()),
                    t!("check.plan_warnings.fix.join_dropped"),
                ),
                JoinCondition::CrossJoin => (
                    t!("check.plan_warnings.title.cross_join", query = capitalize(&query), tense = tense),
                    t!("check.plan_warnings.summary.cross_join"),
                    t!("check.plan_warnings.fix.cross_join"),
                ),
            };
            (f.title, f.summary) = (title, summary);
            f.fix = Some(Fix { summary: fix, sql: None, cost: None });
        }
        Warned::MissingStatistics(statistics) => {
            let StatisticsWarning { insight, stale, now } = statistics.as_ref();
            f.title = t!("check.plan_warnings.title.statistics", query = &query);
            f.summary = t!("check.plan_warnings.summary.statistics");
            f.fix = Some(match insight {
                Some(i) => {
                    f.summary = i.detail.clone();
                    if let Some(stale) = stale {
                        f.title = t!(
                            "check.plan_warnings.title.statistics_on",
                            query = &query,
                            columns = format::list(&stale.columns)
                        );
                        if let Some(now) = now {
                            f.summary = culprit_core::insights::stale_index_statistics_detail(
                                stale,
                                Some(&statistics_now_said(snapshot, now)),
                            );
                        }
                    }
                    Fix { summary: i.fix.clone(), sql: i.fix_sql.clone(), cost: None }
                }
                None => Fix { summary: t!("check.plan_warnings.fix.statistics"), sql: None, cost: None },
            });
        }
    }
    f.verify = vec![if warning == PlanWarning::MissingStatistics {
        t!("check.plan_warnings.verify.statistics")
    } else {
        t!("check.plan_warnings.verify.rescan")
    }];
    // Statements that share one cause: what they take together, said once.
    if let Some(shared) = &case.shared {
        apply_query_impact(&mut f, &shared.impact);
        let column = f.subject.name.clone();
        let tense = if shared.stopped { "past" } else { "present" };
        f.title = if f.confidence == Confidence::High {
            t!(
                "check.plan_warnings.title.shared_stops_seek",
                column = column,
                count = shared.queries.to_string(),
                tense = tense
            )
        } else {
            t!("check.plan_warnings.title.shared", column = column, count = shared.queries.to_string(), tense = tense)
        };
        f.summary = [
            f.summary.clone(),
            t!(
                "check.plan_warnings.summary.shared",
                count = shared.queries.to_string(),
                share = format::percent(shared.impact.share)
            ),
        ]
        .join(" ");
        let mut metrics = vec![
            Metric::new(t!("common.metric.share_of_db_time"), shared.impact.share, Unit::Ratio),
            Metric::new(t!("common.metric.queries_affected"), shared.queries as f64, Unit::Count),
        ];
        if let Some(per_day_ms) = shared.impact.per_day_ms {
            metrics.push(Metric::new(t!("common.metric.db_time_per_day"), per_day_ms, Unit::Ms));
        }
        // The share of logged slow-run time stays: it is of the first statement alone.
        if slow_log_only(snapshot) {
            metrics.push(share_metric);
        }
        metrics.extend(f.metrics.drain(..).skip(2));
        f.metrics = metrics;
    }
    f
}

/// What a type mismatch makes the database do, what SQL Server says it costs, and what the application declares.
fn conversion_summary(c: &Conversion) -> String {
    let column = c.column.clone().unwrap_or_else(|| t!("check.plan_warnings.summary.a_column"));
    let mut summary = vec![
        match &c.expression {
            Some(expression) => t!(
                "check.plan_warnings.summary.convert_expression",
                column = column,
                expression = truncate(expression, 140)
            ),
            None => t!("check.plan_warnings.summary.convert", column = column),
        },
        if c.blocks_seek {
            t!("check.plan_warnings.summary.blocks_seek")
        } else {
            t!("check.plan_warnings.summary.estimates")
        },
    ];
    let unicode = c.target.as_deref().is_some_and(|t| t.to_ascii_lowercase().starts_with("nvarchar"));
    summary.extend(match &c.parameters {
        Some(p) if p.to_ascii_lowercase().contains("nvarchar") => {
            Some(t!("check.plan_warnings.summary.parameters_unicode", parameters = truncate(p, 120)))
        }
        Some(p) => Some(t!("check.plan_warnings.summary.parameters", parameters = truncate(p, 120))),
        None if unicode => Some(t!("check.plan_warnings.summary.unicode")),
        None => None,
    });
    summary.join(" ")
}

/// What the catalog says now of the statistics a plan warned held nothing usable: when each was last updated, from how
/// many rows, and the changes counted since, with the table's rows when the plan was compiled and now. None when the
/// scan read none of them.
fn statistics_now(snapshot: &Snapshot, stale: &StaleIndexStatistics) -> Option<StatisticsNow> {
    let (schema, table) = match stale.table.rsplit_once('.') {
        Some((schema, table)) => (schema.rsplit('.').next(), table),
        None => (None, stale.table.as_str()),
    };
    let same_table =
        |s: &str, t: &str| t.eq_ignore_ascii_case(table) && schema.is_none_or(|schema| s.eq_ignore_ascii_case(schema));
    let indexes: Vec<IndexStatistics> = stale
        .indexes
        .iter()
        .filter_map(|index| {
            let st = snapshot
                .statistics
                .iter()
                .find(|st| same_table(&st.schema, &st.table) && st.name.eq_ignore_ascii_case(index))?;
            Some(IndexStatistics {
                index: index.clone(),
                updated: st.last_updated.clone()?,
                rows: st.rows,
                changes: st.modifications,
            })
        })
        .collect();
    if indexes.is_empty() {
        return None;
    }
    let table_now = snapshot.tables.iter().find(|t| same_table(&t.schema, &t.name)).and_then(|t| t.rows);
    Some(StatisticsNow { indexes, table_then: stale.table_rows, table_now })
}

/// [`StatisticsNow`] in a sentence: each index's statistics, then the table's rows.
fn statistics_now_said(snapshot: &Snapshot, now: &StatisticsNow) -> String {
    let rows = |n: f64| t!("check.plan_warnings.statistics.rows", n = n, count = format::count(n));
    let mut clauses: Vec<String> = now
        .indexes
        .iter()
        .map(|st| {
            let (index, date) = (&st.index, format::friendly_date(&st.updated, &snapshot.taken_at));
            let changes = |m: f64| t!("check.plan_warnings.statistics.changes", n = m, count = format::count(m));
            match (st.rows, st.changes) {
                (Some(n), Some(m)) if m > 0.0 => t!(
                    "check.plan_warnings.statistics.updated_rows_changes",
                    index = index,
                    date = date,
                    rows = rows(n),
                    changes = changes(m)
                ),
                (Some(n), Some(_)) => {
                    t!(
                        "check.plan_warnings.statistics.updated_rows_unchanged",
                        index = index,
                        date = date,
                        rows = rows(n)
                    )
                }
                (Some(n), None) => {
                    t!("check.plan_warnings.statistics.updated_rows", index = index, date = date, rows = rows(n))
                }
                (None, Some(m)) if m > 0.0 => t!(
                    "check.plan_warnings.statistics.updated_changes",
                    index = index,
                    date = date,
                    changes = changes(m)
                ),
                (None, Some(_)) => t!("check.plan_warnings.statistics.updated_unchanged", index = index, date = date),
                (None, None) => t!("check.plan_warnings.statistics.updated", index = index, date = date),
            }
        })
        .collect();
    clauses.extend(match (now.table_then, now.table_now) {
        (Some(then), Some(now)) => {
            Some(t!("check.plan_warnings.statistics.table_then_now", then = rows(then), now = format::count(now)))
        }
        (Some(then), None) => Some(t!("check.plan_warnings.statistics.table_then", then = rows(then))),
        (None, Some(now)) => Some(t!("check.plan_warnings.statistics.table_now", now = rows(now))),
        (None, None) => None,
    });
    t!("check.plan_warnings.statistics.said", clauses = clauses.join("; "))
}

pub(super) fn warning_key(w: PlanWarning) -> &'static str {
    match w {
        PlanWarning::ImplicitConversion => "implicit-conversion",
        PlanWarning::Spill => "spill",
        PlanWarning::NoJoinPredicate => "no-join-predicate",
        PlanWarning::MissingStatistics => "missing-statistics",
        PlanWarning::MemoryGrant => "memory-grant",
        PlanWarning::Misestimate => "misestimate",
        PlanWarning::ResidualPredicate => "residual-predicate",
    }
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

/// What a statement's text says about the join a plan made without a join predicate.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum JoinCondition {
    /// No ON clause, and no comparison of one table's column with another's.
    Missing,
    /// It joins its tables on a condition: the engine dropped it (both sides compared with the same value).
    Present,
    /// It asks for every combination.
    CrossJoin,
}

pub(super) fn join_condition(text: &str) -> JoinCondition {
    let t = format!(" {} ", squeeze(text));
    if t.contains(" cross join ") || t.contains(" cross apply ") {
        return JoinCondition::CrossJoin;
    }
    let joins = t.matches(" join ").count();
    let ons = t.matches(" on ").count() + t.matches(" using(").count() + t.matches(" using ").count();
    if (joins > 0 && ons >= joins) || compares_two_tables(&t) {
        return JoinCondition::Present;
    }
    JoinCondition::Missing
}

/// A comparison of one table's column with another's (`o.customer_id = c.id`): an old-style join in WHERE.
pub(super) fn compares_two_tables(squeezed: &str) -> bool {
    let name = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '@');
    let qualifier = |side: &str| -> Option<String> {
        let (q, column) = side.rsplit_once('.')?;
        let q = q.rsplit('.').next()?;
        let starts = |s: &str| s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        (starts(q) && starts(column)).then(|| q.to_string())
    };
    squeezed.match_indices('=').any(|(at, _)| {
        let before = squeezed[..at].trim_end_matches(['<', '>', '!', ' ']);
        if squeezed[..at].ends_with(['<', '>', '!']) {
            return false;
        }
        let left: String =
            before.chars().rev().take_while(|c| name(*c)).collect::<Vec<_>>().into_iter().rev().collect();
        let right: String = squeezed[at + 1..].trim_start().chars().take_while(|c| name(*c)).collect();
        matches!((qualifier(&left), qualifier(&right)), (Some(a), Some(b)) if a != b)
    })
}
