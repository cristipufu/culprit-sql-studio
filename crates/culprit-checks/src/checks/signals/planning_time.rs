//! Measured statement planning work. Aggregate event counts establish neither plan reuse nor a recompile cause.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};

pub fn planning_time_info() -> CheckInfo {
    CheckInfo {
        id: "planning-time".into(),
        title: t!("check.planning_time.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres],
        requires: vec![],
        description: t!("check.planning_time.info.description"),
    }
}

check!(
    PlanningTimeCheck: planning_time_info,
    decide = planning_times,
    render = planning_time_finding,
    case<'s> = PlanningTime<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Planning)],
);

/// A statement that spends a quarter or more of its time, and five seconds or more in all, being planned.
pub struct PlanningTime<'s> {
    q: &'s QueryStat,
    plan_ms: f64,
    events: Option<f64>,
    average_ms: Option<f64>,
    /// Planning time against planning and running together.
    share: f64,
    step: culprit_core::PlanningStep,
    investigation: PlanningInvestigation,
    severity: Severity,
    memory: Option<culprit_core::CompilationMemory>,
}

#[derive(Clone, Copy)]
enum PlanningInvestigation {
    PreparationAndPlanChoice,
    StatementRecompileDirective,
    CompilationCauseUnknown,
}

fn planning_times(snapshot: &Snapshot) -> Vec<PlanningTime<'_>> {
    let step = snapshot.server.engine.spec().statement_metrics().planning;
    let mut cases: Vec<(PlanningTime<'_>, f64)> = app_queries(snapshot)
        .filter_map(|q| {
            let costs = q.costs.as_ref()?;
            let plan_ms = costs.plan_ms.filter(|ms| *ms >= 5_000.0)?;
            let share = plan_ms / (plan_ms + q.total_time_ms);
            // Compilation/planning and execution aggregates are not a history of individual cache lookups.
            let events = costs.plans.filter(|events| events.is_finite() && *events >= 0.0);
            (share >= 0.25 && q.executions >= 50.0).then_some((q, plan_ms, events, share))
        })
        .map(|(q, plan_ms, events, share)| {
            let severity = if share >= 0.5 && window_at_least(snapshot, q, plan_ms, 60_000.0) {
                Severity::Medium
            } else {
                Severity::Low
            };
            let case = PlanningTime {
                q,
                plan_ms,
                events,
                average_ms: events
                    .filter(|events| *events > 0.0)
                    .map(|events| plan_ms / events)
                    .filter(|average| average.is_finite()),
                share,
                step,
                investigation: if step == culprit_core::PlanningStep::Planning {
                    PlanningInvestigation::PreparationAndPlanChoice
                } else if snapshot.server.engine.sql_syntax() == culprit_core::SqlSyntax::Mssql
                    && culprit_core::sql::has_mssql_recompile_hint(&q.text)
                {
                    PlanningInvestigation::StatementRecompileDirective
                } else {
                    PlanningInvestigation::CompilationCauseUnknown
                },
                severity,
                memory: q.costs.as_ref().and_then(|costs| costs.compile_history.as_ref()?.memory),
            };
            (case, query_score(snapshot, q, severity))
        })
        .collect();
    cases.sort_by(|a, b| b.1.total_cmp(&a.1));
    cases.truncate(5);
    cases.into_iter().map(|(case, _)| case).collect()
}

fn planning_time_finding(snapshot: &Snapshot, case: PlanningTime<'_>) -> Finding {
    let PlanningTime { q, plan_ms, share, step, .. } = case;
    let mut f = finding("planning-time", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    let handle = query_handle(q);
    // PostgreSQL plans a statement, SQL Server compiles it: the messages' `select` words each.
    let is_planning = step == culprit_core::PlanningStep::Planning;
    let work = match step {
        culprit_core::PlanningStep::Planning => "plan",
        culprit_core::PlanningStep::Compiling => "compile",
    };
    f.title = t!(
        "check.planning_time.title",
        work = work,
        tense = tense(snapshot, &[q]),
        share = share_words(share),
        handle = &handle
    );
    let mut summary = vec![t!(
        "check.planning_time.summary.measured",
        work = work,
        run = format::duration(q.mean_time_ms),
        share = share_words(share),
        plan_ms = format::duration(plan_ms),
        ran = ran_phrase(snapshot, q)
    )];
    if let Some(average) = case.average_ms {
        summary.push(t!("check.planning_time.summary.average", work = work, average = format::duration(average)));
    }
    if matches!(case.investigation, PlanningInvestigation::StatementRecompileDirective) {
        summary.push(t!("check.planning_time.summary.recompile_hint"));
    }
    f.summary = summary.join(" ");
    f.severity = case.severity;
    with_query(snapshot, &mut f, q);
    severity_score(&mut f);
    f.impact.set_label(t!("check.planning_time.impact", share = format::percent(share)));
    f.confidence = if is_planning { Confidence::High } else { Confidence::Medium };
    f.metrics = vec![Metric::said(culprit_core::key!("check.planning_time.metric.share"), share, Unit::Ratio)];
    if let Some(average) = case.average_ms {
        let key = if is_planning {
            key!("check.planning_time.metric.per_plan")
        } else {
            key!("check.planning_time.metric.per_compile")
        };
        f.metrics.push(Metric::said(key, average, Unit::Ms));
    }
    if let Some(events) = case.events {
        let key = if is_planning {
            key!("check.planning_time.metric.plans")
        } else {
            key!("check.planning_time.metric.compiles")
        };
        f.metrics.push(Metric::said(key, events, Unit::Count));
    }
    f.metrics.push(Metric::said(key!("common.metric.runs"), q.executions, Unit::Count));
    if let Some(memory) = case.memory.filter(|memory| memory.is_valid() && memory.is_measured()) {
        for (key, kb) in [
            (key!("common.metric.compile_memory_average"), memory.average_kb),
            (key!("common.metric.compile_memory_last"), memory.last_kb),
            (key!("common.metric.compile_memory_maximum"), memory.maximum_kb),
        ] {
            if let Some(kb) = kb {
                f.metrics.push(Metric::said(key, kb * 1024.0, Unit::Bytes));
            }
        }
        f.caveats.push(t!("check.planning_time.caveat.compile_memory"));
    }
    let advice = match case.investigation {
        PlanningInvestigation::PreparationAndPlanChoice => t!("check.planning_time.fix.planning"),
        PlanningInvestigation::StatementRecompileDirective => t!("check.planning_time.fix.recompile_hint"),
        PlanningInvestigation::CompilationCauseUnknown => t!("check.planning_time.fix.compilation_unknown"),
    };
    f.fix = Some(Fix { summary: advice, sql: None, cost: None });
    // Retained planning and execution totals must be compared over equivalent source intervals after a test.
    let now = t!(
        "check.planning_time.verify.now",
        plan_ms = format::duration(plan_ms),
        n = q.executions,
        runs = format::count(q.executions)
    );
    f.facts.advice.running_totals_explained = crate::queries::running_totals(snapshot).is_some();
    f.verify = vec![t!("check.planning_time.verify.comparison")];
    if let Some(source) = crate::queries::running_totals(snapshot) {
        f.verify.insert(0, t!("check.planning_time.verify.between_scans", source = source, now = now));
    }
    f.caveats.push(t!("check.planning_time.caveat.event_counts"));
    if matches!(case.investigation, PlanningInvestigation::StatementRecompileDirective) {
        f.verify.push(t!("check.planning_time.verify.recompile_hint"));
    }
    if step == culprit_core::PlanningStep::Compiling {
        f.caveats.push(t!("check.planning_time.caveat.query_store"));
    }
    f
}

#[cfg(test)]
mod tests;
