use super::*;
use culprit_core::{Engine, QueryWaitCoverage, WaitStat};

fn wait(class: WaitClass, ms: f64) -> WaitStat {
    WaitStat { category: class.category().into(), wait_type: None, wait_ms: ms, tasks: None }
}

#[test]
fn qualified_zero_depends_on_the_declared_class_instead_of_engine_identity() {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    for engine in Engine::ALL {
        snapshot.server.engine = engine;
        let mut query = crate::support::tests::sample();
        query.waits = vec![wait(WaitClass::Io, 1000.0)];
        assert_eq!(RecordedLocks::read(&snapshot, &query), None);
        query.waits_coverage = Some(QueryWaitCoverage { classes: vec![WaitClass::Io], complete: true });
        assert_eq!(RecordedLocks::read(&snapshot, &query), None);
        query.waits_coverage = Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: false });
        assert_eq!(RecordedLocks::read(&snapshot, &query), None);
        query.waits_coverage.as_mut().unwrap().complete = true;
        assert_eq!(RecordedLocks::read(&snapshot, &query).unwrap().total, LockTotal::Measured(0.0));
    }
}

#[test]
fn partial_or_malformed_coverage_keeps_observed_positive_lock_subtotals() {
    let snapshot = crate::waits::snapshot_with(Engine::Mssql);
    for coverage in [
        None,
        Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: false }),
        Some(QueryWaitCoverage { classes: vec![WaitClass::Io], complete: true }),
        Some(QueryWaitCoverage { classes: vec![WaitClass::Lock, WaitClass::Lock], complete: true }),
    ] {
        let mut query = crate::support::tests::sample();
        query.waits = vec![wait(WaitClass::Lock, 10.0), wait(WaitClass::Lock, 12.0)];
        query.waits_coverage = coverage;
        assert_eq!(RecordedLocks::read(&snapshot, &query).unwrap().total, LockTotal::Observed(22.0));
    }
}

#[test]
fn a_native_timer_does_not_establish_whole_class_absence_without_qualification() {
    let snapshot = crate::waits::snapshot_with(Engine::Mysql);
    let mut query = crate::support::tests::sample();
    query.spread = Some(culprit_core::Spread { lock_ms: Some(0.0), ..Default::default() });
    assert_eq!(RecordedLocks::read(&snapshot, &query), None);
    query.spread.as_mut().unwrap().lock_ms = Some(15.0);
    assert_eq!(RecordedLocks::read(&snapshot, &query).unwrap().total, LockTotal::Observed(15.0));
    query.waits_coverage = Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: true });
    assert_eq!(RecordedLocks::read(&snapshot, &query).unwrap().total, LockTotal::Measured(15.0));
    query.spread.as_mut().unwrap().lock_ms = Some(0.0);
    assert_eq!(RecordedLocks::read(&snapshot, &query).unwrap().total, LockTotal::Measured(0.0));
    for ms in [f64::NAN, f64::INFINITY, -1.0] {
        query.spread.as_mut().unwrap().lock_ms = Some(ms);
        query.waits = vec![wait(WaitClass::Lock, ms)];
        assert_eq!(RecordedLocks::read(&snapshot, &query), None);
    }
}

#[test]
fn source_wording_requires_the_declared_workload_kind() {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    snapshot.workload.as_mut().unwrap().kind = None;
    assert_eq!(WaitSource::of(&snapshot), WaitSource::Engine);
    snapshot.workload.as_mut().unwrap().kind = Some(WorkloadKind::QueryStore);
    assert_eq!(WaitSource::of(&snapshot), WaitSource::QueryStore);
    snapshot.server.engine = Engine::Postgres;
    assert_eq!(WaitSource::of(&snapshot), WaitSource::QueryStore);
}
