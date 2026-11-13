//! Compiler admission can delay this database's requests without exhausting all server memory. Recent expensive
//! compiles are investigation candidates; a historical peak never identifies a current compiler-memory owner.
use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::{MeasurementScope, QueryRef, QueryStat, SignalScope, signal_id};

const RECENT_COMPILE_SECONDS: f64 = 5.0 * 60.0;
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
    memory_kb: f64,
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
    let observed_at = format::epoch_seconds(&snapshot.taken_at);
    let mut candidates = app_queries(snapshot)
        .filter_map(|query| {
            let history = query.costs.as_ref()?.compile_history.as_ref()?;
            let compiled_at = history.last_at.as_deref()?;
            let age = observed_at? - format::epoch_seconds(compiled_at)?;
            let memory = history.memory.filter(culprit_core::CompilationMemory::is_valid)?;
            let memory_kb = memory.last_kb.filter(|kb| *kb >= CANDIDATE_MEMORY_KB)?;
            (history.count >= 1.0 && (0.0..=RECENT_COMPILE_SECONDS).contains(&age)).then_some(RecentCompiler {
                query,
                memory_kb,
                compiled_at,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.memory_kb.total_cmp(&a.memory_kb).then_with(|| a.query.id.cmp(&b.query.id)));
    candidates.truncate(MAX_CANDIDATES);
    vec![CompilationQueue { waiting: signal.value, candidates }]
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
                memory = format::bytes(candidate.memory_kb * 1024.0),
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
mod tests {
    use super::*;
    use crate::waits::snapshot_with;
    use culprit_core::{CompilationMemory, Pressure, QueryCosts, Signal, SignalUnit, signals::CompileHistory};

    fn observed_queue() -> Snapshot {
        let mut snapshot = snapshot_with(Engine::Mssql);
        snapshot.taken_at = "2026-11-13T10:00:00Z".into();
        let mut signal =
            Signal::new(signal_id::COMPILATION_WAITING, "fixture", 2.0, SignalUnit::Count, SignalScope::Now);
        signal.measurement_scope = Some(MeasurementScope::Database);
        snapshot.pressure = Some(Pressure { signals: vec![signal], ..Default::default() });
        let query = snapshot
            .workload
            .as_mut()
            .unwrap()
            .queries
            .iter_mut()
            .find(|q| !crate::support::is_monitoring_query(q))
            .unwrap();
        query.costs = Some(QueryCosts {
            compile_history: Some(CompileHistory {
                count: 5.0,
                last_at: Some("2026-11-13T09:59:00Z".into()),
                memory: Some(CompilationMemory {
                    last_kb: Some(32768.0),
                    maximum_kb: Some(1048576.0),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
        snapshot
    }

    #[test]
    fn a_database_compiler_queue_links_recent_memory_heavy_compiles_as_candidates_without_asserting_ownership() {
        let snapshot = observed_queue();
        let findings = CompileQueueCheck.run(&snapshot);
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.title, "2 requests are waiting to compile");
        assert_eq!(finding.severity, Severity::Low);
        assert_eq!(finding.queries.len(), 1);
        let candidate = finding.queries[0].note.as_deref().unwrap();
        assert!(candidate.contains("candidate") && candidate.contains("32.0 MB"), "{candidate}");
        assert!(!candidate.contains("1.1 GB"), "the historical maximum is not the latest compilation");
        assert!(finding.caveats[0].contains("memory holders"));
        assert!(finding.fix.as_ref().unwrap().sql.is_none());
        assert!(finding.summary.contains("slots"));
    }

    #[test]
    fn shared_unknown_execution_or_empty_queues_do_not_establish_database_compiler_pressure() {
        for scope in [None, Some(MeasurementScope::Server), Some(MeasurementScope::Pool)] {
            let mut snapshot = observed_queue();
            snapshot.pressure.as_mut().unwrap().signals[0].measurement_scope = scope;
            assert!(CompileQueueCheck.run(&snapshot).is_empty());
        }
        let mut snapshot = observed_queue();
        snapshot.pressure.as_mut().unwrap().signals[0].key = signal_id::MEMORY_GRANTS_PENDING.into();
        assert!(CompileQueueCheck.run(&snapshot).is_empty(), "execution grants are a different allocator");
        let mut snapshot = observed_queue();
        snapshot.pressure.as_mut().unwrap().signals[0].value = 0.0;
        assert!(CompileQueueCheck.run(&snapshot).is_empty());
    }

    #[test]
    fn old_future_unmeasured_or_peak_only_memory_never_identifies_a_recent_candidate() {
        for (last_at, last_kb) in [
            ("2026-11-13T09:00:00Z", Some(32768.0)),
            ("2026-11-13T10:01:00Z", Some(32768.0)),
            ("2026-11-13T09:59:00Z", None),
            ("2026-11-13T09:59:00Z", Some(f64::NAN)),
        ] {
            let mut snapshot = observed_queue();
            for query in &mut snapshot.workload.as_mut().unwrap().queries {
                if let Some(history) = query.costs.as_mut().and_then(|costs| costs.compile_history.as_mut()) {
                    history.last_at = Some(last_at.into());
                    history.memory.as_mut().unwrap().last_kb = last_kb;
                }
            }
            let finding = CompileQueueCheck.run(&snapshot).remove(0);
            assert!(finding.queries.is_empty());
            assert!(!finding.fix.unwrap().summary.is_empty(), "missing attribution still needs a next step");
        }
    }
}
