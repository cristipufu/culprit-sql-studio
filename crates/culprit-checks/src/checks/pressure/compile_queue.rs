//! Compiler admission can delay this database's requests without exhausting all server memory. Recent expensive
//! compiles are investigation candidates; a historical peak never identifies a current compiler-memory owner.
use super::*;
use crate::{evidence::Requirement as Evidence, views::compilation};
use culprit_core::{
    MeasurementScope, QueryRef, QueryStat, SignalScope, SourceState, signal_id, telemetry::ObservationBounds,
};
use std::time::Duration;

const RECENT_COMPILE_LOOKBACK: Duration = Duration::from_secs(5 * 60);
const CANDIDATE_MEMORY_KB: f64 = 16.0 * 1024.0;
const MAX_CANDIDATES: usize = 5;

pub fn compile_queue_info() -> CheckInfo {
    CheckInfo {
        id: "compile-queue".into(),
        title: t!("check.compile_queue.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.compile_queue.info.description"),
    }
}

check!(
    CompileQueueCheck: compile_queue_info,
    decide = compilation_queues,
    render = compilation_queue_finding,
    case<'s> = CompilationQueue<'s>,
    evidence = &[Evidence::Pressure {
        signal: Some(signal_id::COMPILATION_WAITING), timed_files: false, complete: true
    }],
);

pub struct CompilationQueue<'s> {
    waiting: f64,
    candidates: Vec<RecentCompiler<'s>>,
}

struct RecentCompiler<'s> {
    query: &'s QueryStat,
    memory_bytes: f64,
    compiled_at: &'s str,
}

fn compilation_queues(snapshot: &Snapshot) -> Vec<CompilationQueue<'_>> {
    let Some(signal) = pressure(snapshot).and_then(|pressure| pressure.signal(signal_id::COMPILATION_WAITING)) else {
        return vec![];
    };
    if signal.scope != SignalScope::Now
        || signal.measurement_scope != Some(MeasurementScope::Database)
        || !signal.value.is_finite()
        || signal.value < 1.0
    {
        return vec![];
    }
    vec![CompilationQueue { waiting: signal.value, candidates: recent_compilers(snapshot) }]
}

fn recent_compilers(snapshot: &Snapshot) -> Vec<RecentCompiler<'_>> {
    let Some(compilation::CapturedCompilations { read, source, cases }) =
        compilation::CapturedCompilations::from_snapshot(snapshot)
    else {
        return Vec::new();
    };
    if !source.is_some_and(|source| matches!(source.state, SourceState::Collected | SourceState::Partial)) {
        return Vec::new();
    }
    // The retained context owns native acquisition clocks. Snapshot/read wrapper times are client clocks;
    // neither those clocks nor a query's projected history can establish a recent native compilation.
    let Some(context) = &read.context else { return Vec::new() };
    let mut candidates = cases
        .into_iter()
        .filter_map(|case| {
            let query = compilation::workload_query(snapshot, read, case.native)?;
            if is_monitoring_query(query)
                || case.native.compilations.parse::<u64>().ok()? == 0
                || case.native.seconds_since_first_compile < 0
                || ObservationBounds::new(&case.native.first_compile_at().ok()?, &case.native.last_compile_at).is_none()
            {
                return None;
            }
            let age = ObservationBounds::new(&case.native.last_compile_at, &context.observed_until)?;
            if !age.duration_at_most(RECENT_COMPILE_LOOKBACK) {
                return None;
            }
            let memory_kb = case.native.memory?.last_kb.filter(|kb| *kb >= CANDIDATE_MEMORY_KB)?;
            let memory_bytes = memory_kb * 1024.0;
            memory_bytes.is_finite().then_some(RecentCompiler {
                query,
                memory_bytes,
                compiled_at: &case.native.last_compile_at,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.memory_bytes.total_cmp(&a.memory_bytes).then_with(|| a.query.id.cmp(&b.query.id)));
    candidates.truncate(MAX_CANDIDATES);
    candidates
}

fn compilation_queue_finding(snapshot: &Snapshot, case: CompilationQueue<'_>) -> Finding {
    let mut finding = finding(
        "compile-queue",
        &server_key(snapshot),
        Category::Queries,
        subject(SubjectKind::Database, &snapshot.server.database),
    );
    finding.title = t!("check.compile_queue.title", n = case.waiting, count = format::count(case.waiting));
    finding.summary = t!("check.compile_queue.summary");
    finding.severity = Severity::Low;
    finding.confidence = Confidence::High;
    finding.impact.score = base_score(finding.severity);
    finding.impact.set_label(finding.title.clone());
    finding.metrics.push(Metric::said(key!("check.compile_queue.metric.waiting"), case.waiting, Unit::Count));
    finding.queries = case
        .candidates
        .into_iter()
        .map(|candidate| QueryRef {
            id: candidate.query.id.clone(),
            plan_node: None,
            note: Some(t!(
                "check.compile_queue.candidate",
                memory = format::bytes(candidate.memory_bytes),
                at = candidate.compiled_at
            )),
        })
        .collect();
    finding.fix = Some(Fix { summary: t!("check.compile_queue.fix"), sql: None, cost: None });
    finding.verify = vec![t!("check.compile_queue.verify")];
    finding.caveats = vec![t!("check.compile_queue.caveat")];
    finding
}

#[cfg(test)]
mod tests;
