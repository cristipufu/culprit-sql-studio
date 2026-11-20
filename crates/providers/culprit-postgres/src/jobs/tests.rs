use super::*;
use crate::native::jobs::{bounded_budget, test_support};
fn frozen(io: crate::progress::test_support::Reader) -> FrozenSource<Reader<crate::progress::test_support::Reader>> {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        budget: bounded_budget(&CollectionBudget::default()),
    };
    FrozenSource::new(Reader { io, host: "fixture.invalid".into(), port: 9, verified: None }, &request).unwrap()
}
#[tokio::test]
async fn frozen_pages_and_verification_reuse_the_original_job_read_and_close_once() {
    let mut source = frozen(test_support::reader());
    let description = source.describe().await.unwrap();
    let queries = source.query_count();
    assert!(description.observation.covers_request());
    let page = source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    let CollectionRead::Page { records, next_cursor: None } = page else { panic!("original bounded job page") };
    assert!(records.iter().any(|record| matches!(record, TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::TimescaleJobState(point), ..
    }) if point.configuration_check.as_ref().is_some_and(|check| check.name == "validate_owned_configuration"))));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), queries);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), queries);
}
#[tokio::test]
async fn unconfirmed_close_never_publishes_positive_or_empty_job_evidence() {
    for empty in [false, true] {
        let mut io = test_support::reader();
        io.unconfirmed = true;
        if empty {
            io.replies[2] = Ok(vec![]);
            io.replies[3] = Ok(vec![]);
        }
        let (snapshot, coverage) =
            collect_from(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await;
        assert!(snapshot.is_none());
        assert!(io.closed);
        assert!(coverage.outcomes().iter().all(|source| source.state == culprit_core::SourceState::Unavailable));
    }
}
#[tokio::test]
async fn source_failure_survives_shutdown_failure_without_a_success_or_reconnect() {
    let mut io = test_support::reader();
    io.unconfirmed = true;
    io.replies[2] = Err(CollectionStop::Cancelled.error());
    let (snapshot, coverage) =
        collect_from(&mut io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await;
    assert!(snapshot.is_none());
    assert!(io.closed);
    assert_eq!(io.calls.len(), 3);
    for source in coverage.outcomes() {
        assert_eq!(source.state, culprit_core::SourceState::Unavailable);
        let reason = source.reason.unwrap();
        assert!(reason.contains(&CollectionStop::Cancelled.error().message));
        assert!(reason.contains(&CollectionStop::CancellationUnconfirmed.error().message));
    }
}
