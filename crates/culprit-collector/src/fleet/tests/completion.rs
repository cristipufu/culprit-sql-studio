use super::*;

#[tokio::test]
async fn custom_complete_without_reference_integrity_keeps_the_original_interval_pending() {
    for coverage in [ReferenceCoverage::default(), ReferenceCoverage { integrity_complete: true, ..Default::default() }]
    {
        let root = tempfile::tempdir().unwrap();
        let inventory =
            inventory(ScheduledCollection::QueryHistory { lookback_seconds: 7_200, reconcile_seconds: 7_200 });
        let job = job(&inventory);
        let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
        let mut outbox = outbox(&root.path().join("outbox")).await;
        let backend = Backend::new();
        let cancellation = CollectionCancellation::default();
        let discovered = work::execute(
            &admitted(&backend),
            &journal,
            &mut CaptureStorage::local(&mut outbox),
            &job,
            &cancellation,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(discovered.event.work, FleetWork::DiscoverIntervals);
        *backend.reference_coverage.lock().unwrap() = coverage;
        let refused = work::execute(
            &admitted(&backend),
            &journal,
            &mut CaptureStorage::local(&mut outbox),
            &job,
            &cancellation,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(refused.event.status, CollectionStatus::Incomplete(CollectionStop::InvalidPage));
        assert!(matches!(refused.outcome, scheduler::Outcome::Failed));
        assert!(!refused.pause_all);
        assert_eq!(refused.source, None);
        assert_eq!(journal.gaps(None, 10).await.unwrap().gaps.len(), 0);
        *backend.reference_coverage.lock().unwrap() =
            crate::collection_references::ObservationReferences::default().coverage(true, None);
        let accepted = work::execute(
            &admitted(&backend),
            &journal,
            &mut CaptureStorage::local(&mut outbox),
            &job,
            &cancellation,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(accepted.event.status, CollectionStatus::Complete);
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1], "an unauthenticated Complete label cannot consume a discovered interval");
        assert_eq!(*backend.discoveries.lock().unwrap(), 1);
    }
}

#[tokio::test]
async fn physically_complete_native_rows_can_advance_without_claiming_semantic_resolution() {
    let root = tempfile::tempdir().unwrap();
    let inventory = inventory(ScheduledCollection::QueryHistory { lookback_seconds: 7_200, reconcile_seconds: 7_200 });
    let job = job(&inventory);
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    let cancellation = CollectionCancellation::default();
    work::execute(&admitted(&backend), &journal, &mut CaptureStorage::local(&mut outbox), &job, &cancellation, now())
        .await
        .unwrap();
    *backend.reference_coverage.lock().unwrap() = ReferenceCoverage {
        integrity_complete: true,
        queries: DefinitionCoverage { required: 1, resolved: 0, unresolved: 1 },
        ..Default::default()
    };
    for _ in 0..2 {
        let result = work::execute(
            &admitted(&backend),
            &journal,
            &mut CaptureStorage::local(&mut outbox),
            &job,
            &cancellation,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(result.event.status, CollectionStatus::Complete);
    }
    {
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_ne!(requests[0], requests[1]);
    }
    assert!(journal.load(&job.key).await.unwrap().history.is_none());
}
