//! The `planning-time` check: Statements where planning (PostgreSQL, with pg_stat_statements.track_planning on) or compiling (SQL Server Query Store) takes a large part of their time, usually because the plan isn't reused.

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
    plans: f64,
    /// Planning time against planning and running together.
    share: f64,
    /// PostgreSQL plans; SQL Server compiles.
    pg: bool,
    /// A new plan for (nearly) every run.
    every_run: bool,
    /// SQL Server's OPTION (RECOMPILE) asks for a new plan on every run.
    recompile_hint: bool,
    severity: Severity,
}

fn planning_times(snapshot: &Snapshot) -> Vec<PlanningTime<'_>> {
    let pg = snapshot.server.engine == Engine::Postgres;
    let mut cases: Vec<(PlanningTime<'_>, f64)> = app_queries(snapshot)
        .filter_map(|q| {
            let costs = q.costs.as_ref()?;
            let plan_ms = costs.plan_ms.filter(|ms| *ms >= 5_000.0)?;
            let share = plan_ms / (plan_ms + q.total_time_ms);
            (share >= 0.25 && q.executions >= 50.0).then_some((q, plan_ms, costs.plans.unwrap_or(0.0), share))
        })
        .map(|(q, plan_ms, plans, share)| {
            let severity = if share >= 0.5 && window_at_least(snapshot, q, plan_ms, 60_000.0) {
                Severity::Medium
            } else {
                Severity::Low
            };
            let case = PlanningTime {
                q,
                plan_ms,
                plans,
                share,
                pg,
                every_run: plans >= q.executions * 0.9,
                recompile_hint: !pg && culprit_core::sql::has_mssql_recompile_hint(&q.text),
                severity,
            };
            (case, query_score(snapshot, q, severity))
        })
        .collect();
    cases.sort_by(|a, b| b.1.total_cmp(&a.1));
    cases.truncate(5);
    cases.into_iter().map(|(case, _)| case).collect()
}

fn planning_time_finding(snapshot: &Snapshot, case: PlanningTime<'_>) -> Finding {
    let PlanningTime { q, plan_ms, plans, share, pg, .. } = case;
    let mut f = finding("planning-time", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    let handle = query_handle(q);
    // PostgreSQL plans a statement, SQL Server compiles it: the messages' `select` words each.
    let work = if pg { "plan" } else { "compile" };
    f.title = t!(
        "check.planning_time.title",
        work = work,
        tense = tense(snapshot, &[q]),
        share = share_words(share),
        handle = &handle
    );
    let per_plan = per_run(plan_ms, plans.max(1.0));
    let (each, runs, all, ran) = (
        format::duration(per_plan),
        format::duration(q.mean_time_ms),
        format::duration(plan_ms),
        ran_phrase(snapshot, q),
    );
    let mut summary = vec![if case.every_run {
        t!(
            "check.planning_time.summary.every_run",
            work = work,
            per_plan = each,
            run = runs,
            share = share_words(share),
            plan_ms = all,
            ran = ran
        )
    } else {
        t!(
            "check.planning_time.summary.reused",
            work = work,
            per_plan = each,
            run = runs,
            share = share_words(share),
            plan_ms = all,
            ran = ran,
            reuses = format::count(per_run(q.executions, plans.max(1.0)))
        )
    }];
    if case.recompile_hint {
        summary.push(t!("check.planning_time.summary.recompile_hint"));
    }
    f.summary = summary.join(" ");
    f.severity = case.severity;
    with_query(snapshot, &mut f, q);
    severity_score(&mut f);
    f.impact.set_label(t!("check.planning_time.impact", share = format::percent(share)));
    f.confidence = if pg { Confidence::High } else { Confidence::Medium };
    f.metrics = vec![
        Metric::said(culprit_core::key!("check.planning_time.metric.share"), share, Unit::Ratio),
        Metric::said(culprit_core::key!("check.planning_time.metric.per_plan"), per_plan, Unit::Ms),
        Metric::said(culprit_core::key!("check.planning_time.metric.plans"), plans, Unit::Count),
        Metric::said(key!("common.metric.runs"), q.executions, Unit::Count),
    ];
    f.fix = Some(if pg {
        Fix { summary: t!("check.planning_time.fix.prepare"), sql: None, cost: None }
    } else if case.recompile_hint {
        Fix { summary: t!("check.planning_time.fix.recompile_hint"), sql: None, cost: None }
    } else if case.every_run {
        Fix {
            summary: t!("check.planning_time.fix.every_run"),
            sql: Some("-- Database-wide: statements that differ only in literals share a plan\nALTER DATABASE CURRENT SET PARAMETERIZATION FORCED;".into()),
            cost: None,
        }
    } else {
        Fix { summary: t!("check.planning_time.fix.slow_compiles"), sql: None, cost: None }
    });
    // On running totals the planning time holds every plan from before the change: two scans show it.
    let now = t!(
        "check.planning_time.verify.now",
        plan_ms = format::duration(plan_ms),
        n = q.executions,
        runs = format::count(q.executions)
    );
    f.verify = vec![match crate::queries::running_totals(snapshot) {
        Some(source) => t!("check.planning_time.verify.between_scans", source = source, now = now),
        None => t!("check.planning_time.verify.rescan"),
    }];
    if case.recompile_hint {
        f.verify.push(t!("check.planning_time.verify.recompile_hint"));
    }
    if !pg {
        f.caveats.push(t!("check.planning_time.caveat.query_store"));
    }
    f
}
