use super::*;

#[tokio::test]
async fn a_complete_original_history_prefix_cannot_skip_its_unread_suffix() {
    let (mut original, _) = crate::analytical::test_support::managed_instance_resource_evidence();
    let CollectionTarget::Shared(resource) = &original.target else { panic!() };
    let mut inventory = inventory(ScheduledCollection::ResourceHistory { window_seconds: 60, overlap_seconds: 0 });
    inventory.targets[0].subject = CollectionSubject::Shared { resource: resource.clone() };
    let job = job(&inventory);
    let root = tempfile::tempdir().unwrap();
    let journal = FleetJournal::open(&root.path().join("state"), &inventory, "one").await.unwrap();
    let mut outbox = outbox(&root.path().join("outbox")).await;
    let backend = Backend::new();
    let cancellation = CollectionCancellation::default();
    let SourceObservation::MssqlManagedInstanceResources(history) = &mut original.observation else { panic!() };
    history.coverage = StateSourceCoverage::observed(history.coverage.source.clone(), 1, false);
    history.coverage.stopped = Some(CollectionStop::RowLimit);
    history.native_rows = 2;
    assert!(original.observation.covers_request(), "the partial original remains valid for offline analysis");
    *backend.description_override.lock().unwrap() = Some(original.clone());

    let first = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &cancellation,
        work::parse_time("2020-01-01T06:01:00Z").unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(first.event.status, CollectionStatus::Complete);
    assert_eq!(first.event.native_coverage, Some(culprit_core::SourceState::Partial));
    assert!(matches!(first.outcome, scheduler::Outcome::Failed));
    assert_eq!(first.source, Some(original.clone()));
    let checkpoint = journal.load(&job.key).await.unwrap();
    assert!(checkpoint.last_until.is_none());
    assert!(checkpoint.pending.is_some());

    let SourceObservation::MssqlManagedInstanceResources(history) = &mut original.observation else { panic!() };
    history.coverage = StateSourceCoverage::collected(history.coverage.source.clone(), 1);
    history.native_rows = 1;
    *backend.description_override.lock().unwrap() = Some(original);
    let retry = work::execute(
        &admitted(&backend),
        &journal,
        &mut CaptureStorage::local(&mut outbox),
        &job,
        &cancellation,
        work::parse_time("2020-01-01T06:02:00Z").unwrap(),
    )
    .await
    .unwrap();
    assert!(matches!(retry.outcome, scheduler::Outcome::Complete));
    assert_eq!(retry.event.native_coverage, Some(culprit_core::SourceState::Collected));
    let checkpoint = journal.load(&job.key).await.unwrap();
    assert!(checkpoint.pending.is_none());
    assert_eq!(checkpoint.last_until.as_deref(), Some("2020-01-01T06:01:00+00:00"));
    let requests = backend.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1], "retry must read the exact original window before advancing");
}

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
