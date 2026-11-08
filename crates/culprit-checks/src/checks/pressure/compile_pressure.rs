//! Instance-wide compilation context, only when this database also has literal query families to inspect.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn compile_pressure_info() -> CheckInfo {
    CheckInfo {
        id: "compile-pressure".into(),
        title: t!("check.compile_pressure.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.compile_pressure.info.description"),
    }
}

check!(
    CompilePressureCheck: compile_pressure_info,
    decide = compile_context,
    render = compile_pressure_finding,
    case<'s> = CompileContext,
    evidence = &[Evidence::Pressure {
        signal: Some(culprit_core::signal_id::COMPILES_PER_BATCH),
        timed_files: false,
        complete: false
    }],
);

/// The compile-pressure findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn compile_pressure(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&CompilePressureCheck, snapshot)
}

/// The instance's compile ratio since it started, as context for this database's literal query families: it doesn't
/// attribute the server's cost to this database.
pub struct CompileContext {
    /// Compiles per 100 batches.
    ratio: f64,
    /// Batches a second, averaged since the start.
    batches: f64,
    /// Compiles a minute.
    per_min: f64,
    /// Recompiles per 100 batches, when there were any.
    recompiles: Option<f64>,
}

fn compile_context(snapshot: &Snapshot) -> Vec<CompileContext> {
    let Some(compiles) = pressure(snapshot).and_then(|p| p.signal(culprit_core::signal_id::COMPILES_PER_BATCH)) else {
        return Vec::new();
    };
    // Shared counters alone establish no compilation problem here; retained only as context when this database's own literal families make inspection worthwhile. Query Store supplies the database's measured compile costs.
    if snapshot.server.on(culprit_core::Platform::AzureSqlDatabase) || snapshot.query_families.is_empty() {
        return Vec::new();
    }
    let ratio = compiles.value;
    // The load behind the ratio, averaged over at least an hour since the start.
    let (Some(batches), Some(_)) = (signal(snapshot, culprit_core::signal_id::BATCH_REQUESTS), counter_hours(snapshot))
    else {
        return Vec::new();
    };
    if ratio < 15.0 || batches < 5.0 {
        return Vec::new();
    }
    vec![CompileContext {
        ratio,
        batches,
        per_min: batches * ratio / 100.0 * 60.0,
        recompiles: signal(snapshot, culprit_core::signal_id::RECOMPILES_PER_BATCH).filter(|r| *r > 0.0),
    }]
}

fn compile_pressure_finding(snapshot: &Snapshot, c: CompileContext) -> Finding {
    let mut f = finding(
        "compile-pressure",
        &server_key(snapshot),
        Category::Configuration,
        subject(SubjectKind::Database, snapshot.server.database.clone()),
    );
    let percent = format!("{:.0}", c.ratio);
    f.title = t!("check.compile_pressure.title", percent = &percent, per_minute = format::count(c.per_min));
    f.summary = t!(
        "check.compile_pressure.summary",
        since = uptime_opening(snapshot),
        batches = format::count(c.batches),
        percent = &percent
    );
    // Historical instance-wide context, without assigning the server's cost to this database.
    f.severity = Severity::Low;
    f.impact.score = base_score(f.severity) + c.ratio / 5.0;
    f.impact.label = t!("check.compile_pressure.impact", percent = &percent);
    f.confidence = Confidence::Medium;
    f.metrics = vec![
        Metric::new(t!("check.compile_pressure.metric.compiles"), c.ratio, Unit::Count),
        Metric::new(t!("check.compile_pressure.metric.per_minute"), c.per_min, Unit::Count),
    ];
    if let Some(recompiles) = c.recompiles {
        f.metrics.push(Metric::new(t!("check.compile_pressure.metric.recompiles"), recompiles, Unit::Count));
    }
    f.fix = Some(Fix { summary: t!("check.compile_pressure.fix"), sql: None, cost: None });
    f.verify = vec![t!("check.compile_pressure.verify")];
    f
}
