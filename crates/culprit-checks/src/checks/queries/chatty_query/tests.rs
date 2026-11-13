use super::*;
use culprit_core::{QueryWaitCoverage, WaitStat, telemetry::WaitClass};

fn counter() -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    let workload = snapshot.workload.as_mut().unwrap();
    workload.queries.truncate(1);
    let query = &mut workload.queries[0];
    query.text = "UPDATE dbo.counters SET n = n + 1 OUTPUT inserted.n WHERE name = @P1".into();
    (query.executions, query.mean_time_ms, query.total_time_ms) = (30_000.0, 1.0, 30_000.0);
    query.rows = Some(30_000.0);
    query.rows_affected = None;
    query.first_seen = None;
    query.last_seen = None;
    query.waits.clear();
    query.waits_coverage = None;
    query.spread = None;
    workload.total_time_ms = query.total_time_ms;
    workload.total_executions = query.executions;
    workload.window_hours = 1.0;
    snapshot
}

fn wait(class: WaitClass, ms: f64) -> WaitStat {
    WaitStat { category: class.category().into(), wait_type: None, wait_ms: ms, tasks: None }
}

fn case(snapshot: &Snapshot) -> ChattyQuery<'_> {
    let mut cases = chatty_queries(snapshot);
    assert_eq!(cases.len(), 1);
    cases.pop().unwrap()
}

#[test]
fn a_sparse_other_class_never_adds_zero_lock_time_to_a_counter_finding() {
    let mut snapshot = counter();
    snapshot.workload.as_mut().unwrap().queries[0].waits = vec![wait(WaitClass::Io, 1000.0)];
    let case = case(&snapshot);
    assert_eq!(case.locks, None);
    let finding = chatty_query_finding(&snapshot, case);
    assert!(!finding.summary.contains("lock waits"), "{}", finding.summary);
}

#[test]
fn the_decision_keeps_qualified_zero_separate_from_observed_positive_lock_time() {
    let mut snapshot = counter();
    snapshot.workload.as_mut().unwrap().queries[0].waits_coverage =
        Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: true });
    let measured = case(&snapshot);
    assert_eq!(measured.locks.unwrap().total, LockTotal::Measured(0.0));
    let finding = chatty_query_finding(&snapshot, measured);
    assert!(finding.summary.contains("recorded 0 ms of lock waits"), "{}", finding.summary);

    let query = &mut snapshot.workload.as_mut().unwrap().queries[0];
    query.waits_coverage.as_mut().unwrap().complete = false;
    query.waits = vec![wait(WaitClass::Lock, 1000.0)];
    let observed = case(&snapshot);
    assert_eq!(observed.locks.unwrap().total, LockTotal::Observed(1000.0));
    let finding = chatty_query_finding(&snapshot, observed);
    assert!(finding.summary.contains("At least 1.0 s of lock waits"), "{}", finding.summary);
    assert!(finding.summary.contains("incomplete"), "{}", finding.summary);
}

#[test]
fn a_successful_native_projection_does_not_assign_query_store_from_sql_server_identity() {
    let mut snapshot = counter();
    let workload = snapshot.workload.as_mut().unwrap();
    workload.kind = None;
    workload.queries[0].waits_coverage = Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: true });
    let measured = case(&snapshot);
    assert_eq!(measured.locks.unwrap().source, WaitSource::Engine);
    let finding = chatty_query_finding(&snapshot, measured);
    assert!(finding.summary.contains("SQL Server counted 0 ms of lock waits"), "{}", finding.summary);
    assert!(!finding.summary.contains("Query Store recorded"), "{}", finding.summary);
}
