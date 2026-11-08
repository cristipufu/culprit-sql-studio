//! The `jit-overhead` check: Statements where PostgreSQL's JIT compilation (pg_stat_statements jit_* columns, 15+) takes a large part of their run time.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};

pub fn jit_overhead_info() -> CheckInfo {
    CheckInfo {
        id: "jit-overhead".into(),
        title: t!("check.jit_overhead.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Postgres],
        requires: vec![],
        description: t!("check.jit_overhead.info.description"),
    }
}

check!(
    JitOverheadCheck: jit_overhead_info,
    decide = jit_overheads,
    render = jit_overhead_finding,
    case<'s> = JitOverhead<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Jit)],
);

/// A statement that spends a quarter or more of its time, and a second or more in all, compiling with JIT.
pub struct JitOverhead<'s> {
    q: &'s QueryStat,
    jit: f64,
    /// JIT time against the statement's own time.
    share: f64,
    /// jit_above_cost as Culprit's session read it.
    threshold: Option<&'s str>,
    fix: JitFix,
    severity: Severity,
}

/// What the fix does about the threshold.
#[derive(Clone, Copy)]
pub enum JitFix {
    /// The threshold Culprit read isn't a number it can raise from: turn JIT off for the application's role.
    RoleOff,
    /// Raise jit_above_cost from what Culprit read to `target`, or turn JIT off for the role instead.
    Raise { current: f64, target: f64 },
}

/// The default jit_above_cost, and the threshold the fix raises it to from there: ten times as high.
pub(super) const JIT_ABOVE_COST_DEFAULT: f64 = 100_000.0;

fn jit_overheads(snapshot: &Snapshot) -> Vec<JitOverhead<'_>> {
    let threshold = snapshot.setting("jit_above_cost").map(|s| s.value.as_str());
    let mut cases: Vec<(JitOverhead<'_>, f64)> = app_queries(snapshot)
        .filter_map(|q| {
            let jit = q.costs.as_ref()?.jit_ms.filter(|ms| *ms >= 1_000.0)?;
            let share = per_run(jit, q.total_time_ms);
            (share >= 0.25).then_some((q, jit, share))
        })
        .map(|(q, jit, share)| {
            // Medium needs both, as planning time does: most of the statement's time, and a minute a day of it.
            let severity = if share >= 0.5 && window_at_least(snapshot, q, jit, 60_000.0) {
                Severity::Medium
            } else {
                Severity::Low
            };
            let case = JitOverhead { q, jit, share, threshold, fix: jit_fix(threshold), severity };
            (case, query_score(snapshot, q, severity))
        })
        .collect();
    cases.sort_by(|a, b| b.1.total_cmp(&a.1));
    cases.truncate(5);
    cases.into_iter().map(|(case, _)| case).collect()
}

/// Raise jit_above_cost where this server is configured, from what it is now: ten times the default, or ten times the
/// current value when that is already higher, never lower. Or turn JIT off for the application's role.
fn jit_fix(current: Option<&str>) -> JitFix {
    match current.and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && *v >= 0.0) {
        Some(current) => JitFix::Raise { current, target: (current.max(JIT_ABOVE_COST_DEFAULT) * 10.0).round() },
        None => JitFix::RoleOff,
    }
}

fn jit_overhead_finding(snapshot: &Snapshot, case: JitOverhead<'_>) -> Finding {
    let JitOverhead { q, jit, share, .. } = case;
    let mut f = finding("jit-overhead", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    let handle = query_handle(q);
    // A statement that stopped running is told in the past.
    let tense = tense(snapshot, &[q]);
    f.title = if share >= 0.5 {
        t!("check.jit_overhead.title.longer", tense = tense, handle = &handle)
    } else {
        t!("check.jit_overhead.title.share", tense = tense, share = format::percent(share), handle = &handle)
    };
    f.summary = [
        t!(
            "check.jit_overhead.summary.spent",
            per_run = format::duration(per_run(jit, q.executions)),
            run = format::duration(q.mean_time_ms),
            jit = format::duration(jit),
            ran = ran_phrase(snapshot, q)
        ),
        match case.threshold {
            Some(threshold) => t!("check.jit_overhead.summary.threshold", threshold = threshold),
            None => t!("check.jit_overhead.summary.threshold_unread"),
        },
        t!("check.jit_overhead.summary.pays_off"),
    ]
    .join(" ");
    f.severity = case.severity;
    with_query(snapshot, &mut f, q);
    severity_score(&mut f);
    f.impact.label = t!("check.jit_overhead.impact", share = format::percent(share));
    f.confidence = Confidence::High;
    f.metrics = vec![
        Metric::new(t!("check.jit_overhead.metric.share"), share, Unit::Ratio),
        Metric::new(t!("check.jit_overhead.metric.per_run"), per_run(jit, q.executions), Unit::Ms),
        Metric::new(t!("check.jit_overhead.metric.run_time"), q.mean_time_ms, Unit::Ms),
    ];
    let role = "-- For the role the application connects as (takes effect on its next connections):\nALTER ROLE <app_role> SET jit = off;";
    f.fix = Some(match case.fix {
        JitFix::RoleOff => Fix {
            summary: t!("check.jit_overhead.fix.role_off"),
            sql: Some(role.into()),
            cost: Some(t!("check.jit_overhead.fix.role_off_cost")),
        },
        JitFix::Raise { current, target } => {
            let change = setting_change(&snapshot.server, "jit_above_cost", &format!("{target:.0}"));
            Fix {
                summary: t!("check.jit_overhead.fix.raise", current = format::count(current), how = &change.how),
                sql: Some(match change.sql {
                    Some(sql) => format!(
                        "-- Only if the application uses the threshold Culprit read, with no role, database or session override:\n{sql}\n-- Alternative, for the application's role only:\n-- ALTER ROLE <app_role> SET jit = off;"
                    ),
                    None => role.to_string(),
                }),
                cost: None,
            }
        }
    });
    // On running totals the JIT time holds every run from before the change: two scans show it.
    let now = t!("check.jit_overhead.verify.now", jit = format::duration(jit), runs = format::count(q.executions));
    f.verify = vec![match crate::queries::running_totals(snapshot) {
        Some(source) => t!("check.jit_overhead.verify.between_scans", source = source, now = now),
        None => t!("check.jit_overhead.verify.rescan"),
    }];
    f
}
