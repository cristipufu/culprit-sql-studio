//! The `tuning-advice` check: Plan-forcing and index recommendations from automatic tuning, forced plans (those that stopped applying, and those that scan where an index added since could seek), Query Store hints that stopped applying, and automatic plan correction left off.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::{AutomaticTuningOption, QueryHint, t};

mod forced_plans;
mod index_advice;

pub(crate) use index_advice::recommended_create;

use forced_plans::{FailureHint, Forced, ForcedPlans, ForcedScan, Forcing, forced_plans_finding, forced_scan_finding};
use index_advice::{
    CreateIndex, DropIndex, create_index_advice, create_index_finding, drop_index_advice, drop_index_finding,
};

pub fn tuning_advice_info() -> CheckInfo {
    CheckInfo {
        id: "tuning-advice".into(),
        title: t!("check.tuning_advice.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql],
        requires: vec![Capability::QueryStore],
        description: t!("check.tuning_advice.info.description"),
    }
}

check!(
    TuningAdviceCheck: tuning_advice_info,
    decide = tuning_cases,
    render = tuning_finding,
    case<'s> = TuningCase<'s>,
    evidence = &[Evidence::Source(culprit_core::source_id::TUNING)],
);

/// What SQL Server's own tuning state establishes, one finding each.
pub enum TuningCase<'s> {
    /// Azure SQL's recommendation to create an index nothing else has.
    CreateIndex(CreateIndex<'s>),
    /// Azure SQL's recommendation to drop an index nothing else flags.
    DropIndex(DropIndex<'s>),
    /// A plan SQL Server recommends forcing, waiting for someone to apply it.
    ForcePlan(ForcePlan<'s>),
    /// A forced plan SQL Server could not use.
    ForcingFails(ForcingFails<'s>),
    /// A forced plan that scans a table an index could seek.
    ForcedScan(ForcedScan<'s>),
    /// The forced plans that apply.
    ForcedPlans(ForcedPlans<'s>),
    /// A Query Store hint SQL Server could not apply.
    HintFails(&'s QueryHint),
    /// Automatic plan correction set on, but not running.
    CorrectionNotRunning(&'s AutomaticTuningOption),
    /// Automatic plan correction off while this many plan fixes wait.
    CorrectionOff(usize),
}

/// A plan-forcing recommendation still waiting to be applied.
pub struct ForcePlan<'s> {
    r: &'s TuningRecommendation,
    query_id: &'s str,
    plan_id: &'s str,
    query: Option<&'s QueryStat>,
    /// The time the query took in the window, where it is among its statements.
    impact: Option<QueryImpact>,
    severity: Severity,
    /// Not among the window's statements, and Query Store says it hasn't run for over a day: (seconds, last run).
    gone: Option<(f64, &'s str)>,
    /// Automatic plan correction is on: what forcing by hand means then, instead of advice to turn it on.
    correcting: bool,
}

/// A forced plan SQL Server could not use, and why.
pub struct ForcingFails<'s> {
    plan: &'s ForcedPlan,
    query: Option<&'s QueryStat>,
    hint: Option<FailureHint>,
    gone: Option<(f64, &'s str)>,
    severity: Severity,
}

/// A day or more between `last` and the end of the scan's data, as (seconds, `last`).
pub(super) fn quiet_since<'a>(snapshot: &Snapshot, last: Option<&'a str>) -> Option<(f64, &'a str)> {
    let last = last?;
    let end = snapshot.workload.as_ref()?.window_end.as_deref().and_then(parse_epoch_seconds)?;
    let silence = end - parse_epoch_seconds(last)?;
    (silence >= 86_400.0).then_some((silence, last))
}

/// Waiting for someone to apply it. SQL Server applied the others itself (Verifying, Success) or let them go
/// (Reverted, Expired): a plan it forced shows among the forced plans instead.
pub(super) fn waiting(r: &TuningRecommendation) -> bool {
    r.state.as_deref().is_none_or(|s| matches!(s.to_ascii_lowercase().as_str(), "active" | "pending"))
}

/// A plan-forcing recommendation still waiting to be applied.
pub(super) fn waiting_plan_fix(r: &TuningRecommendation) -> bool {
    waiting(r) && r.query_id.is_some() && r.recommended_plan_id.is_some()
}

fn tuning_cases(snapshot: &Snapshot) -> Vec<TuningCase<'_>> {
    let Some(tuning) = &snapshot.tuning else { return Vec::new() };
    let mut out = Vec::new();

    for r in tuning.recommendations.iter().filter(|r| waiting(r)) {
        let Some(index) = &r.index else { continue };
        if r.kind.eq_ignore_ascii_case("CreateIndex") {
            out.extend(create_index_advice(snapshot, r, index).map(TuningCase::CreateIndex));
        } else if r.kind.eq_ignore_ascii_case("DropIndex") {
            out.extend(drop_index_advice(snapshot, r, index).map(TuningCase::DropIndex));
        }
    }

    for r in tuning.recommendations.iter().filter(|r| waiting(r)) {
        let (Some(query_id), Some(plan_id)) = (&r.query_id, &r.recommended_plan_id) else { continue };
        let query = snapshot.workload.as_ref().and_then(|w| w.query(query_id));
        let impact = query.and_then(|q| query_impact(snapshot, &[q]));
        out.push(TuningCase::ForcePlan(ForcePlan {
            r,
            query_id,
            plan_id,
            query,
            severity: match &impact {
                Some(impact) => severity_for_impact(snapshot, query.as_slice(), impact),
                // SQL Server recommends from about ten seconds of estimated CPU saved: with no time in the window
                // behind it (the query is outside the top statements, or newer), it is Low until the workload shows
                // the time.
                None => Severity::Low,
            },
            impact,
            // Not among the window's queries, and Query Store says it hasn't run for over a day: most likely a
            // deploy replaced the statement, and the advice is about code that is gone.
            gone: query.is_none().then(|| quiet_since(snapshot, r.last_execution.as_deref())).flatten(),
            // Turning automatic plan correction on is advice only where it is off: on, it applies its own
            // recommendations, and forcing by hand is what the note says it is.
            correcting: automatic_plan_correction(snapshot) == Some(true),
        }));
    }

    for p in tuning.forced_plans.iter().filter(|p| p.failures > 0.0) {
        let query = snapshot.workload.as_ref().and_then(|w| w.query(&p.query_id));
        out.push(TuningCase::ForcingFails(ForcingFails {
            plan: p,
            query,
            hint: p.last_failure_reason.as_deref().and_then(FailureHint::of),
            gone: query.is_none().then(|| quiet_since(snapshot, p.last_execution.as_deref())).flatten(),
            severity: if p.failures >= 100.0 { Severity::Medium } else { Severity::Low },
        }));
    }

    // Forced plans that apply: SQL Server runs each as it is, also after an index is added or the data grows.
    let mut kept: Vec<Forced<'_>> = Vec::new();
    for p in tuning.forced_plans.iter().filter(|p| p.failures <= 0.0) {
        let q = snapshot.workload.as_ref().and_then(|w| w.query(&p.query_id));
        let applied = tuning.recommendations.iter().find(|r| {
            r.query_id.as_deref() == Some(p.query_id.as_str())
                && r.recommended_plan_id.as_deref() == Some(p.plan_id.as_str())
        });
        let automatic = p.forcing.as_deref().is_some_and(|f| f.eq_ignore_ascii_case("AUTO"))
            || applied.is_some_and(|r| !waiting(r));
        let forced_at = applied.and_then(|r| r.applied_at.as_deref());
        let how = match automatic {
            true => Forcing::Automatic(forced_at),
            false if p.forcing.as_deref().is_some_and(|f| f.eq_ignore_ascii_case("MANUAL")) => Forcing::Hand,
            false => Forcing::Unknown,
        };
        match q.and_then(|q| forced_plans::seekable_scan(snapshot, q, &p.plan_id).map(|scan| (q, scan))) {
            Some((q, scan)) => out.push(TuningCase::ForcedScan(ForcedScan::of(snapshot, p, q, how, forced_at, scan))),
            None => kept.push(Forced {
                plan: p,
                query: q,
                how,
                quiet: q
                    .is_none()
                    .then(|| quiet_since(snapshot, p.last_execution.as_deref()))
                    .flatten()
                    .map(|(_, l)| l),
            }),
        }
    }
    if !kept.is_empty() {
        // Automatic plan correction keeps checking the plans it forced and unforces one that turns out slower, while
        // it is on; a plan forced by hand (or once it's off) stays forced until someone unforces it.
        let correcting = crate::tuning::automatic_plan_correction(snapshot) == Some(true);
        out.push(TuningCase::ForcedPlans(ForcedPlans { kept, correcting }));
    }

    for h in tuning.hints.iter().filter(|h| h.failures > 0.0) {
        out.push(TuningCase::HintFails(h));
    }

    if let Some(plan) = tuning.automatic_tuning.iter().find(|o| o.name.eq_ignore_ascii_case("FORCE_LAST_GOOD_PLAN")) {
        let desired_on = plan.desired.eq_ignore_ascii_case("ON");
        let actual_on = plan.actual.eq_ignore_ascii_case("ON");
        let waiting_fixes = tuning.recommendations.iter().filter(|r| waiting_plan_fix(r)).count();
        if desired_on && !actual_on {
            out.push(TuningCase::CorrectionNotRunning(plan));
        } else if !actual_on && waiting_fixes > 0 {
            out.push(TuningCase::CorrectionOff(waiting_fixes));
        }
    }
    out
}

fn tuning_finding(snapshot: &Snapshot, case: TuningCase<'_>) -> Finding {
    match case {
        TuningCase::CreateIndex(c) => create_index_finding(snapshot, c),
        TuningCase::DropIndex(d) => drop_index_finding(snapshot, d),
        TuningCase::ForcePlan(p) => force_plan_finding(snapshot, p),
        TuningCase::ForcingFails(p) => forcing_fails_finding(snapshot, p),
        TuningCase::ForcedScan(scan) => forced_scan_finding(snapshot, scan),
        TuningCase::ForcedPlans(plans) => forced_plans_finding(snapshot, plans),
        TuningCase::HintFails(h) => hint_fails_finding(h),
        TuningCase::CorrectionNotRunning(plan) => correction_not_running_finding(snapshot, plan),
        TuningCase::CorrectionOff(waiting) => correction_off_finding(snapshot, waiting),
    }
}

/// A query's handle, or "query 123" for one that isn't among the window's statements.
fn statement(query: Option<&QueryStat>, id: &str) -> String {
    query.map(query_handle).unwrap_or_else(|| t!("check.tuning_advice.query", query = id))
}

/// The statement isn't in the window and hasn't run for a day: most likely a deploy replaced it, and the advice is
/// about code that is gone.
fn gone_caveat(f: &mut Finding, query_id: &str, (silence, last): (f64, &str)) {
    f.inactive = true;
    f.impact.score *= 0.25;
    f.last_seen = Some(last.to_string());
    f.caveats.insert(
        0,
        t!(
            "check.tuning_advice.caveat.not_run_since",
            query = query_id,
            last = last.get(..16).unwrap_or(last).replace('T', " "),
            silence = format::duration(silence * 1000.0)
        ),
    );
}

fn force_plan_finding(_snapshot: &Snapshot, p: ForcePlan<'_>) -> Finding {
    let ForcePlan { r, query_id, plan_id, query, .. } = p;
    let mut f = finding(
        "tuning-advice",
        &format!("force:{query_id}"),
        Category::Queries,
        subject(SubjectKind::Query, query_id.to_string()),
    );
    // SQL Server's gain compares the two plans' average CPU per run, not their duration.
    f.title = t!(
        "check.tuning_advice.force.title",
        plan = plan_id,
        statement = statement(query, query_id),
        gained = if r.estimated_gain.is_some() { "yes" } else { "no" },
        gain = r.estimated_gain.map(format::percent).unwrap_or_default()
    );
    f.summary = [
        t!(
            "check.tuning_advice.force.summary",
            query = query_id,
            from = r.regressed_plan_id.as_deref().unwrap_or("?"),
            plan = plan_id,
            dated = if r.valid_since.is_some() { "yes" } else { "no" },
            since = r
                .valid_since
                .as_deref()
                .map(|s| s.replace('T', " ").trim_end_matches('Z').to_string())
                .unwrap_or_default()
        ),
        if r.error_prone {
            t!("check.tuning_advice.force.error_prone")
        } else {
            t!("check.tuning_advice.force.reversible")
        },
    ]
    .join(" ");
    f.severity = p.severity;
    match &p.impact {
        Some(impact) => apply_query_impact(&mut f, impact),
        None => {
            f.impact.score = base_score(f.severity);
            f.impact.label = t!("check.tuning_advice.force.impact");
        }
    }
    f.confidence = Confidence::High;
    if let Some(g) = r.estimated_gain {
        f.metrics.push(Metric::new(t!("check.tuning_advice.metric.cpu_saved"), g, Unit::Ratio));
    }
    f.metrics.push(Metric::text(t!("check.tuning_advice.metric.recommended_plan"), plan_id));
    f.facts.recommended_plan =
        Some(culprit_core::FindingPlanRecommendation { plan: plan_id.to_string(), cpu_saved_share: r.estimated_gain });
    if let Some(bad) = &r.regressed_plan_id {
        f.metrics.push(Metric::text(t!("check.tuning_advice.metric.current_plan"), bad.clone()));
    }
    if let Some(q) = query {
        f.queries = vec![QueryRef { id: q.id.clone(), plan_node: None, note: None }];
    } else if let Some(gone) = p.gone {
        gone_caveat(&mut f, query_id, gone);
    }
    let automatic = match p.correcting {
        true => t!("check.tuning_advice.hand_forcing_note"),
        false => t!("check.tuning_advice.force.fix_turn_on_correction"),
    };
    f.fix = Some(Fix {
        summary: [t!("check.tuning_advice.force.fix"), automatic].join(" "),
        sql: Some(r.script.clone().unwrap_or_else(|| {
            format!("EXEC sp_query_store_force_plan @query_id = {query_id}, @plan_id = {plan_id};")
        })),
        cost: None,
    });
    f.verify = vec![
        t!("check.tuning_advice.force.verify", query = query_id, plan = plan_id),
        t!(
            "check.tuning_advice.force.verify_undo",
            sql = format!("EXEC sp_query_store_unforce_plan @query_id = {query_id}, @plan_id = {plan_id};")
        ),
    ];
    f
}

fn forcing_fails_finding(_snapshot: &Snapshot, failing: ForcingFails<'_>) -> Finding {
    let p = failing.plan;
    let mut f = finding(
        "tuning-advice",
        &format!("forced-failing:{}", p.query_id),
        Category::Queries,
        subject(SubjectKind::Query, p.query_id.to_string()),
    );
    let reason = p.last_failure_reason.clone().unwrap_or_else(|| t!("check.tuning_advice.unknown_reason"));
    let count = format::count(p.failures);
    f.title = t!(
        "check.tuning_advice.forcing_fails.title",
        statement = statement(failing.query, &p.query_id),
        count = &count,
        reason = &reason
    );
    // The hint is its own sentence; without one, the summary ends where it did.
    f.summary = [
        t!(
            "check.tuning_advice.forcing_fails.summary",
            plan = &p.plan_id,
            query = &p.query_id,
            count = &count,
            reason = &reason
        ),
        failing.hint.map(FailureHint::said).unwrap_or_default(),
    ]
    .join(" ");
    f.severity = failing.severity;
    f.impact.score = base_score(f.severity) + p.failures.log10().max(0.0);
    f.impact.label = t!("check.tuning_advice.forcing_fails.impact", count = &count);
    f.confidence = Confidence::High;
    f.metrics.push(Metric::new(t!("check.tuning_advice.metric.forcing_failures"), p.failures, Unit::Count));
    f.metrics.push(Metric::text(t!("check.tuning_advice.metric.last_reason"), reason));
    if let Some(q) = failing.query {
        f.queries = vec![QueryRef { id: q.id.clone(), plan_node: None, note: None }];
    } else if let Some(gone) = failing.gone {
        gone_caveat(&mut f, &p.query_id, gone);
    }
    f.fix = Some(Fix {
        summary: t!("check.tuning_advice.forcing_fails.fix"),
        sql: Some(format!("EXEC sp_query_store_unforce_plan @query_id = {}, @plan_id = {};", p.query_id, p.plan_id)),
        cost: None,
    });
    f.verify = vec![t!("check.tuning_advice.forcing_fails.verify", plan = &p.plan_id)];
    f
}

fn hint_fails_finding(h: &QueryHint) -> Finding {
    let mut f = finding(
        "tuning-advice",
        &format!("hint-failing:{}", h.query_id),
        Category::Queries,
        subject(SubjectKind::Query, h.query_id.to_string()),
    );
    let reason = h.last_failure_reason.clone().unwrap_or_else(|| t!("check.tuning_advice.unknown_reason"));
    let count = format::count(h.failures);
    f.title = t!("check.tuning_advice.hint_fails.title", query = &h.query_id, count = &count, reason = reason);
    f.summary = t!("check.tuning_advice.hint_fails.summary", hint = h.hint.trim(), query = &h.query_id, count = &count);
    f.severity = Severity::Low;
    f.impact.score = base_score(f.severity);
    f.impact.label = t!("check.tuning_advice.hint_fails.impact", count = &count);
    f.confidence = Confidence::High;
    f.fix = Some(Fix {
        summary: t!("check.tuning_advice.hint_fails.fix"),
        sql: Some(format!("EXEC sys.sp_query_store_clear_hints @query_id = {};", h.query_id)),
        cost: None,
    });
    f.verify = vec![t!("check.tuning_advice.hint_fails.verify")];
    f
}

fn correction_finding() -> Finding {
    let mut f = finding(
        "tuning-advice",
        "automatic-plan-correction",
        Category::Configuration,
        subject(SubjectKind::Setting, "FORCE_LAST_GOOD_PLAN"),
    );
    f.impact.label = t!("common.impact.setting");
    f
}

fn correction_not_running_finding(snapshot: &Snapshot, plan: &AutomaticTuningOption) -> Finding {
    let db = quote(Engine::Mssql, &snapshot.server.database);
    let mut f = correction_finding();
    f.title = t!("check.tuning_advice.correction_not_running.title");
    f.summary = t!(
        "check.tuning_advice.correction_not_running.summary",
        actual = &plan.actual,
        reasoned = if plan.reason.is_some() { "yes" } else { "no" },
        reason = plan.reason.as_deref().unwrap_or_default()
    );
    f.severity = Severity::Medium;
    f.impact.score = base_score(f.severity);
    f.confidence = Confidence::High;
    f.fix = Some(Fix {
        summary: t!("check.tuning_advice.correction_not_running.fix"),
        sql: Some(format!("ALTER DATABASE {db} SET QUERY_STORE (OPERATION_MODE = READ_WRITE);")),
        cost: None,
    });
    f.verify = vec![t!("check.tuning_advice.correction_not_running.verify")];
    f
}

fn correction_off_finding(snapshot: &Snapshot, waiting: usize) -> Finding {
    let db = quote(Engine::Mssql, &snapshot.server.database);
    let mut f = correction_finding();
    f.title = t!("check.tuning_advice.correction_off.title");
    f.summary = t!("check.tuning_advice.correction_off.summary", n = waiting, count = waiting.to_string());
    f.severity = Severity::Low;
    f.impact.score = base_score(f.severity) + 4.0;
    f.confidence = Confidence::Medium;
    f.fix = Some(Fix {
        summary: t!("check.tuning_advice.correction_off.fix"),
        sql: Some(format!("ALTER DATABASE {db} SET AUTOMATIC_TUNING (FORCE_LAST_GOOD_PLAN = ON);")),
        cost: None,
    });
    f.verify = vec![t!("check.tuning_advice.correction_off.verify")];
    f
}
