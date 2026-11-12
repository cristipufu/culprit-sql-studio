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
        assert_eq!(sample.minimum_wait_ms, if kind == QueueKind::WorkerTasks { 100 } else { 0 });
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
    for (kind, accepted) in [(QueueKind::MemoryGrants, true), (QueueKind::WorkerTasks, false)] {
        let queued = replace(&row(), &[("confirmed_count", Val::Int(1)), ("longest_wait_ms", Val::Int(50))]);
        let mut reader = FakeRead::new(vec![Ok(vec![queued])]);
        assert_eq!(read(&mut reader, kind).await.is_ok(), accepted);
    }
}
