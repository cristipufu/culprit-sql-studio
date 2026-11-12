//! Plan insights as findings: the plan-level problems in queries that matter, beyond what `plan-warnings`,
//! `filtered-scan` and `missing-index` already report. One finding per problem and table, however many queries share it.

use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::insights::PlanInsight;
use culprit_core::{
    Capability, Category, CheckInfo, Confidence, Engine, Finding, Fix, Metric, Plan, PlanKind, QueryRef, QueryStat,
    Severity, Snapshot, SubjectKind, Unit, format, t,
};

use crate::support::*;

/// Rules whose findings other checks already produce, with the evidence and wording users know. A plan's own
/// missing-index request isn't one of them: `missing-index` reads the server's list of requests, which a restart
/// empties, so the plan's request is a finding of its own until refine finds a `missing-index` or `filtered-scan`
/// finding for the same table and key ("a plan's index request is the finding for its table and key").
const COVERED: &[&str] = &["implicit-conversion", "spill", "no-join-predicate", "missing-statistics", "scan-filter"];

/// Queries below this share of database time aren't worth a finding, however odd their plan.
pub(crate) const MIN_SHARE: f64 = 0.01;

/// The engine's own verdict, shown on a plan without costs to say how much it matters (SQLite builds an index for itself
/// on every run): Low on the plan alone, weighed here by the statement's share of database time.
const WEIGHED_BY_THE_STATEMENT: &[&str] = &["automatic-index"];

pub fn plan_insights_info() -> CheckInfo {
    CheckInfo {
        id: "plan-insights".into(),
        title: t!("check.plan_insights.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql, Engine::Sqlite],
        requires: vec![Capability::QueryPlans],
        description: t!("check.plan_insights.info.description"),
    }
}

check!(
    PlanInsightsCheck: plan_insights_info,
    decide = plan_insight_cases,
    render = plan_insight_finding,
    case<'s> = PlanInsightCase<'s>,
    evidence = &[Evidence::Query(QueryMeasure::CapturedPlans)],
);

/// One plan problem on one table (or in one statement's plan as a whole), and every statement whose plan shows it.
pub struct PlanInsightCase<'s> {
    /// The first statement whose plan shows it, that plan, and the insight as that plan states it.
    query: &'s QueryStat,
    plan: &'s Plan,
    insight: PlanInsight,
    /// `<rule>:<table>`, or `<rule>:query-<id>` for an insight about the whole plan.
    key: String,
    /// The table the insight's step reads, when it names one.
    object: Option<String>,
    severity: Severity,
    /// Every statement whose plan shows it, the first one first, with the step it points at.
    steps: Vec<(&'s QueryStat, Option<u32>)>,
    /// What they take together, when the workload says.
    impact: Option<QueryImpact>,
    /// How many statements those are.
    queries: usize,
}

/// The plan-insights findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn plan_insights(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&PlanInsightsCheck, snapshot)
}

fn plan_insight_cases(snapshot: &Snapshot) -> Vec<PlanInsightCase<'_>> {
    cases_for_queries(snapshot, app_queries(snapshot))
}

/// Rebuild a grouped observation from selected statements while retaining the snapshot's original workload,
/// catalog and measurement context. Refinement never clones or narrows the database-time denominator.
pub(crate) fn plan_insights_for_queries<'a>(
    snapshot: &'a Snapshot,
    queries: impl IntoIterator<Item = &'a QueryStat>,
) -> Vec<Finding> {
    cases_for_queries(snapshot, queries).into_iter().map(|case| plan_insight_finding(snapshot, case)).collect()
}

/// The plan problems the statements' plans show that matter, one case per problem and table.
fn cases_for_queries<'s>(
    snapshot: &'s Snapshot,
    queries: impl IntoIterator<Item = &'s QueryStat>,
) -> Vec<PlanInsightCase<'s>> {
    let mut out: Vec<PlanInsightCase> = Vec::new();
    for q in queries {
        let Some(plan) = &q.plan else { continue };
        let Some(impact) = query_impact(snapshot, &[q]) else { continue };
        if impact.share < MIN_SHARE {
            continue;
        }
        for i in crate::views::plan_advice::catalog_insights(snapshot, q, plan) {
            let weighed_here = WEIGHED_BY_THE_STATEMENT.contains(&i.rule.as_str());
            if COVERED.contains(&i.rule.as_str()) || (i.severity == Severity::Low && !weighed_here) {
                continue;
            }
            // An insight about the whole plan names no step of it, nor the table of the root it points at: it is the
            // query's.
            let step = i.nodes.first().copied().filter(|_| !whole_plan(plan, &i));
            let object = plan.nodes().into_iter().find(|n| Some(n.id) == step).and_then(|n| n.object.clone());
            let key = format!(
                "{}:{}",
                i.rule,
                object.as_deref().map(str::to_ascii_lowercase).unwrap_or_else(|| format!("query-{}", q.id))
            );
            match out.iter_mut().find(|c| c.key == key) {
                Some(existing) => {
                    if !existing.steps.iter().any(|(r, _)| r.id == q.id) {
                        existing.steps.push((q, step));
                    }
                }
                None => {
                    let severity = severity_of(snapshot, q, &i);
                    out.push(PlanInsightCase {
                        query: q,
                        plan,
                        insight: i,
                        key,
                        object,
                        severity,
                        steps: vec![(q, step)],
                        impact: None,
                        queries: 0,
                    });
                }
            }
        }
    }
    for case in &mut out {
        let queries: Vec<&QueryStat> =
            case.steps.iter().filter_map(|(r, _)| snapshot.workload.as_ref()?.query(&r.id)).collect();
        case.impact = query_impact(snapshot, &queries);
        case.queries = queries.len();
    }
    out
}

/// A plan problem matters as much as the query it slows down: a 1% query never becomes a top finding, and one that
/// costs seconds a day is Low whatever its share.
fn severity_of(snapshot: &Snapshot, q: &QueryStat, i: &PlanInsight) -> Severity {
    let share = query_impact(snapshot, &[q]).map_or(0.0, |impact| impact.share);
    let by_share = match severity_for_share(share) {
        Severity::Low => Severity::Medium,
        _ => Severity::High,
    };
    // What that share comes to a day, on the basis it is on (Culprit's scans where they measured it).
    let cap = by_share.min(amount_severity_of(snapshot, &[q], share));
    // An engine verdict the plan can't weigh (SQLite's automatic index) takes the statement's weight, at most Medium:
    // the plan doesn't say how much of the run the throwaway index costs.
    if WEIGHED_BY_THE_STATEMENT.contains(&i.rule.as_str()) { cap.min(Severity::Medium) } else { i.severity.min(cap) }
}

fn plan_insight_finding(_: &Snapshot, case: PlanInsightCase<'_>) -> Finding {
    let PlanInsightCase { query: q, plan, insight: i, .. } = case;
    let actual = plan.kind == PlanKind::Actual;
    let subject = match &case.object {
        Some(o) => subject(SubjectKind::Table, o.clone()),
        None => subject(SubjectKind::Query, q.id.clone()),
    };
    let mut f = finding("plan-insights", &case.key, Category::Queries, subject);
    f.facts.workload_dependency = Some(culprit_core::WorkloadDependency::CurrentStatements);
    f.facts.cause = match i.rule.as_str() {
        "non-sargable" | "leading-wildcard" => Some(culprit_core::FindingCause::PredicateRewrite),
        "big-sort" | "temp-table-sort" => Some(culprit_core::FindingCause::Sort),
        "spill" => Some(culprit_core::FindingCause::Spill),
        "missing-index-hint" => Some(culprit_core::FindingCause::IndexRequest),
        "automatic-index" => Some(culprit_core::FindingCause::AutomaticIndex),
        _ => None,
    };
    f.severity = case.severity;
    f.confidence = if actual { Confidence::High } else { Confidence::Medium };
    f.title = i.title.clone();
    let mut summary = vec![i.detail.clone(), t!("check.plan_insights.summary.seen_in", query = query_handle(q))];
    let label = rule_label(&i);
    f.queries = case
        .steps
        .iter()
        .map(|(r, step)| QueryRef { id: r.id.clone(), plan_node: *step, note: Some(label.clone()) })
        .collect();
    // A share of the plan's work only where the plan states one (`insights::stated_share`): a plan without costs
    // (SQLite) has none, a rule's weight is no measurement, and a cheap plan's or a guessed step's share isn't where the
    // statement's work is.
    if let Some(impact) = &case.impact {
        apply_query_impact(&mut f, impact);
        f.metrics.push(Metric::said(culprit_core::key!("common.metric.share_of_db_time"), impact.share, Unit::Ratio));
        if case.queries > 1 {
            summary.push(t!(
                "check.plan_insights.summary.shared",
                count = case.queries.to_string(),
                share = format::percent(impact.share)
            ));
            f.metrics.push(Metric::said(
                culprit_core::key!("common.metric.queries_affected"),
                case.queries as f64,
                Unit::Count,
            ));
        }
    }
    f.summary = summary.join(" ");
    if let Some(share) = culprit_core::insights::stated_share(plan, &i) {
        f.metrics.push(Metric::said(culprit_core::key!("check.plan_insights.metric.plan_share"), share, Unit::Ratio));
    }
    f.metrics.push(Metric::said(culprit_core::key!("common.metric.average_duration"), q.mean_time_ms, Unit::Ms));
    f.fix = Some(Fix { summary: i.fix.clone(), sql: i.fix_sql.clone(), cost: None });
    f.verify = vec![i.verify.clone(), t!("check.plan_insights.verify.rescan")];
    if culprit_core::insights::table_gone(plan, &i).is_some() {
        f.facts.nothing_to_check = Some(culprit_core::NothingToCheck::Gone);
    }
    if !actual && plan.weighed() {
        f.caveats.push(t!("check.plan_insights.caveat.estimated"));
    }
    f
}

/// Rules about the whole plan that weigh it by a time of their own (compiling it, JIT-compiling it) rather than a share
/// of a step's cost.
const TIMED_WHOLE_PLAN: &[&str] = &["slow-compile", "jit"];

/// An insight about the plan as a whole (it runs on one CPU, the optimizer stopped searching, its compile, a grant for
/// the whole run): it points at the plan's root with a rule's own weight, or a time of its own, not at a step whose
/// share of the cost is the problem.
fn whole_plan(plan: &culprit_core::Plan, i: &PlanInsight) -> bool {
    i.nodes.first() == Some(&plan.root.id) && (i.fixed_weight || TIMED_WHOLE_PLAN.contains(&i.rule.as_str()))
}

fn rule_label(i: &PlanInsight) -> String {
    culprit_core::insights::RULES
        .iter()
        .find(|r| r.0 == i.rule)
        .map(|r| r.2.to_string())
        .unwrap_or_else(|| i.rule.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(name: &str) -> culprit_core::Report {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn findings_are_complete_capped_and_not_duplicated() {
        for name in ["mssql-demo.report.json", "pg-demo.report.json", "mysql-demo.report.json"] {
            let snapshot = report(name).snapshot;
            let found = plan_insights(&snapshot);
            for f in &found {
                assert!(f.id.starts_with("plan-insights:"));
                assert!(!COVERED.iter().any(|c| f.id.starts_with(&format!("plan-insights:{c}:"))), "{name}: {}", f.id);
                assert!(f.fix.is_some() && f.verify.len() == 2 && !f.queries.is_empty(), "{name}: {}", f.id);
                assert!(f.impact.db_time_share.is_some());
            }
            let mut ids: Vec<&String> = found.iter().map(|f| &f.id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), found.len(), "{name}: one finding per problem and table");
        }
    }

    #[test]
    fn an_insight_about_the_whole_plan_names_no_step() {
        // Query 263 runs on one CPU (MaxDOPSetToOne): the plan's root it points at isn't where the problem is.
        let snapshot = report("mssql-demo.report.json").snapshot;
        let found = plan_insights(&snapshot);
        let blocked = found
            .iter()
            .find(|f| f.id.starts_with("plan-insights:parallelism-blocked:"))
            .unwrap_or_else(|| panic!("{:?}", found.iter().map(|f| &f.id).collect::<Vec<_>>()));
        assert_eq!(blocked.id, "plan-insights:parallelism-blocked:query-263");
        assert!(blocked.queries.iter().all(|r| r.plan_node.is_none()), "{:?}", blocked.queries);
        // A rule about a step still names it.
        assert!(
            found.iter().filter(|f| !f.id.contains(":parallelism-blocked:")).all(|f| f.queries[0].plan_node.is_some())
        );
    }
}
