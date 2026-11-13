use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::SourceState;

pub(crate) fn row() -> Rec {
    Rec::of(&[("confirmed_count", Val::Int(0)), ("unattributed_count", Val::Int(0)), ("longest_wait_ms", Val::Null)])
}
#[tokio::test]
async fn queue_gauges_keep_exact_counts_native_units_thresholds_and_unknown_wait_duration() {
    for kind in KINDS {
        let queued = replace(&row(), &[("confirmed_count", Val::Int(9_007_199_254_740_993))]);
        let mut reader = FakeRead::new(vec![Ok(vec![queued])]);
        let sample = read(&mut reader, kind).await.unwrap();
        assert_eq!(sample.count, "9007199254740993");
        assert_eq!(sample.kind, kind);
        assert_eq!(sample.scope, MeasurementScope::Database);
        assert_eq!(sample.longest_wait_ms, None);
        assert_eq!(sample.minimum_wait_ms, minimum_wait_ms(kind));
        assert!(sample.attribution_complete());
        assert!(observation(sample).is_valid());
    }
}
#[tokio::test]
async fn a_failed_or_missing_aggregate_never_becomes_an_observed_zero_queue() {
    for replies in [
        Ok(vec![]),
        Ok(vec![row(), row()]),
        Ok(vec![replace(&row(), &[("confirmed_count", Val::Null)])]),
        Ok(vec![replace(&row(), &[("confirmed_count", Val::Float(1.0))])]),
        Ok(vec![replace(&row(), &[("unattributed_count", Val::Int(-1))])]),
        Ok(vec![replace(&row(), &[("longest_wait_ms", Val::Int(100))])]),
        Err(ReadError::Stopped(CollectionStop::TimedOut)),
    ] {
        let mut reader = FakeRead::new(vec![replies]);
        let result = read(&mut reader, QueueKind::WorkerTasks).await;
        assert!(result.is_err());
        let mut coverage = Coverage::default();
        assert!(record_application_result(&mut coverage, QueueKind::WorkerTasks, result).is_none());
        assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
        assert_eq!(reader.calls.len(), 1, "failures are not retried or converted to empty reads");
    }
}
#[tokio::test]
async fn unattributed_work_does_not_establish_a_clean_database_queue() {
    for confirmed in [0, 1] {
        let queued = replace(&row(), &[("confirmed_count", Val::Int(confirmed)), ("unattributed_count", Val::Int(2))]);
        let mut reader = FakeRead::new(vec![Ok(vec![queued])]);
        let sample = read(&mut reader, QueueKind::MemoryGrants).await.unwrap();
        assert!(!sample.attribution_complete());
        let mut coverage = Coverage::default();
        let signal = record_application_result(&mut coverage, QueueKind::MemoryGrants, Ok(sample));
        assert_eq!(
            signal.is_some(),
            confirmed > 0,
            "confirmed positive work remains evidence; unknown scope is not healthy zero"
        );
        assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
        assert!(coverage.outcomes()[0].reason.is_some());
    }
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let sample = read(&mut reader, QueueKind::MemoryGrants).await.unwrap();
    let mut coverage = Coverage::default();
    assert!(record_application_result(&mut coverage, QueueKind::MemoryGrants, Ok(sample)).is_some());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Collected);
}
#[tokio::test]
async fn native_wait_threshold_is_part_of_the_measurement_not_an_invented_zero_or_request_count() {
    for (kind, accepted) in
        [(QueueKind::MemoryGrants, true), (QueueKind::WorkerTasks, false), (QueueKind::CompilationRequests, false)]
    {
        let queued = replace(&row(), &[("confirmed_count", Val::Int(1)), ("longest_wait_ms", Val::Int(50))]);
        let mut reader = FakeRead::new(vec![Ok(vec![queued])]);
        assert_eq!(read(&mut reader, kind).await.is_ok(), accepted);
    }
}

#[tokio::test]
async fn compilation_admission_counts_requests_in_this_database_separately_from_execution_grants() {
    let queued = replace(&row(), &[("confirmed_count", Val::Int(2)), ("longest_wait_ms", Val::Int(4000))]);
    let mut reader = FakeRead::new(vec![Ok(vec![queued])]);
    let sample = read(&mut reader, QueueKind::CompilationRequests).await.unwrap();
    assert_eq!(sample.minimum_wait_ms, 1000);
    assert_eq!(sample.scope, MeasurementScope::Database);
    assert_eq!(sample.longest_wait_ms.as_deref(), Some("4000"));
    let query = &reader.calls[0];
    assert!(query.sql.contains("RESOURCE_SEMAPHORE_QUERY_COMPILE"));
    assert!(query.sql.contains("r.database_id=DB_ID()"));
    assert!(!query.sql.contains("sys.dm_exec_query_memory_grants"));
    assert!(matches!(query.parameters.as_slice(), [Parameter::Integer(1000)]));
    let signal = record_application_result(&mut Coverage::default(), sample.kind, Ok(sample)).unwrap();
    assert_eq!(signal.key, culprit_core::signal_id::COMPILATION_WAITING);
    assert_eq!(signal.measurement_scope, Some(MeasurementScope::Database));
}

#[tokio::test]
async fn an_unconfirmed_queue_reader_discards_all_fresh_gauges_and_marks_each_source_unavailable() {
    for shutdown in
        [culprit_core::telemetry::SourceShutdown::Quiescent, culprit_core::telemetry::SourceShutdown::Unconfirmed]
    {
        let mut reader = FakeRead::new(KINDS.iter().map(|_| Ok(vec![row()])).collect());
        reader.shutdown = shutdown;
        let mut coverage = Coverage::default();
        let signals = collect_queues(reader, "fixture permission hint".into(), &mut coverage).await;
        if shutdown == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            assert!(signals.is_empty(), "uncertain native work cannot establish even a healthy zero queue");
            assert!(coverage.outcomes().iter().all(|source| source.state == SourceState::Unavailable));
        } else {
            assert_eq!(signals.len(), KINDS.len());
            assert!(coverage.outcomes().iter().all(|source| source.state == SourceState::Collected));
        }
    }
}
