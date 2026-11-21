//! Actual controller/scheduler/Outbox ownership with controlled native facts; no test approves native source cost.
use super::*;
use crate::{
    analytical::test_support::{temporary_resource_basis, temporary_resource_result},
    fleet::investigations::tests::support::capture_frame,
};

pub(in crate::fleet::investigations) fn frame(focus: &SourceReadFocus, execution: &SourceReadExecutionScope) -> Frame {
    let (source, records) = temporary_resource_result(focus, execution, culprit_core::SourceState::Collected);
    Frame { source, records }
}
async fn fixture(qualified: bool, boundary: bool) -> Harness {
    fixture_with_age(qualified, boundary, Some("3600")).await
}
async fn fixture_with_age(qualified: bool, boundary: bool, age: Option<&str>) -> Harness {
    let (source, mut records) = temporary_resource_basis();
    for record in &mut records {
        if let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlCleanupHorizon(horizon),
            ..
        }) = record
        {
            if let Some(transaction) = &mut horizon.oldest_confirmed {
                transaction.age_seconds = age.map(str::to_owned);
            }
        }
    }
    if !boundary {
        for record in &mut records {
            if let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::MssqlCleanupHorizon(row),
                ..
            }) = record
            {
                row.oldest_confirmed = None;
            }
        }
    }
    let mut harness = Harness::with_activity(Frame { source, records }).await;
    harness.backend.recipe.id = "fixture.temporary-resource-context".into();
    harness.backend.recipe.selector = SourceReadQuestion::TemporarySpaceContributors.selector();
    let RecipeCostQualification::Measured(receipt) = &mut harness.backend.recipe.cost else { panic!() };
    receipt.recipe_id = harness.backend.recipe.id.clone();
    receipt.selector = harness.backend.recipe.selector;
    receipt.scope = RecipeCostScope::NativeRead(
        SourceReadExecutionScope::from_context(
            SourceReadQuestion::TemporarySpaceContributors,
            &NativeReadContext::from_snapshot(&harness.backend.activity.source).unwrap(),
        )
        .unwrap(),
    );
    receipt.tested_budget.max_snapshot_rows = 200;
    receipt.tested_budget.page_rows = 32;
    if !qualified {
        harness.backend.recipe.cost = RecipeCostQualification::Unqualified
    }
    harness
}
async fn episodes(runtime: &Runtime) -> Vec<DiagnosticEpisode> {
    runtime
        .fixture
        .outbox
        .diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 8)
        .await
        .unwrap()
        .checkpoints
        .into_iter()
        .flat_map(|checkpoint| checkpoint.episodes)
        .collect()
}
async fn episode(runtime: &Runtime) -> DiagnosticEpisode {
    episodes(runtime)
        .await
        .into_iter()
        .find(|episode| {
            matches!(
                episode.signal,
                DiagnosticSignal::OriginalObservation {
                    condition: DiagnosticOriginalCondition::MssqlCleanupBoundary { .. },
                    ..
                }
            )
        })
        .unwrap()
}

async fn latest_history_episode(runtime: &Runtime, episode_id: &str) -> DiagnosticEpisode {
    use crate::outbox::{CompletedCaptureBudget, CompletedCaptureLookup};
    let status = runtime.fixture.outbox.status(100, None).await.unwrap();
    assert!(status.next_cursor.is_none(), "the controlled retained history fits this complete selection");
    let latest = status
        .entries
        .into_iter()
        .filter(|entry| entry.request.collection() == TelemetryCollection::Diagnostics)
        .max_by_key(|entry| entry.capture.revision.as_ref().unwrap().sequence.parse::<u64>().unwrap())
        .unwrap();
    let operation = culprit_core::limits::OperationGuard::new(std::time::Duration::from_secs(5)).unwrap();
    let expected = episode_id.to_owned();
    runtime
        .fixture
        .outbox
        .delivery()
        .with_completed_diagnostics(
            CompletedCaptureLookup { connection: &latest.connection, observation_id: &latest.capture.observation_id },
            CompletedCaptureBudget::default(),
            operation.handle(),
            (),
            move |capture| {
                Ok(capture.records().iter().find_map(|row| match &row.record {
                    TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::DiagnosticRevision(revision),
                        ..
                    }) if revision.episode.id == expected => Some(revision.episode.clone()),
                    _ => None,
                }))
            },
        )
        .await
        .unwrap()
        .expect("the immutable recovery revision survives retirement from the working set")
}
async fn complete(runtime: &mut Runtime) -> (DiagnosticCaptureRef, DiagnosticEpisode) {
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Complete);
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(input.rows.len(), input.source.observation.snapshot().unwrap().records as usize);
    runtime.finish(Outcome::FocusedComplete { result: capture.clone() }).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let answer = episode(runtime).await;
    (capture, answer)
}
async fn recover(runtime: &mut Runtime) {
    let activity = &mut runtime.fixture.backend.activity;
    for record in &mut activity.records {
        if let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlCleanupHorizon(row),
            ..
        }) = record
        {
            row.oldest_confirmed = None;
        }
    }
    let snapshot = match &mut activity.source.observation {
        SourceObservation::LocalResource { snapshot, .. } => snapshot,
        _ => panic!(),
    };
    snapshot.observed_from = "2020-01-01T12:00:20Z".into();
    snapshot.observed_until = "2020-01-01T12:00:21Z".into();
    // Other independently clocked native facts are not borrowed into this complete horizon negative.
    activity.records.retain(|record| {
        matches!(
            record,
            TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::MssqlCleanupHorizon(_), .. })
        )
    });
    snapshot.records = activity.records.len() as u32;
    snapshot.sources.retain(|coverage| coverage.source == culprit_core::source_id::OPEN_TRANSACTIONS);
    let original = capture_frame(&mut runtime.fixture.outbox, activity.clone()).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    assert!(!original.observation_id.is_empty());
}

#[tokio::test]
async fn cleanup_signal_enrolls_one_qualified_question_and_retains_multiple_nonexclusive_native_contexts() {
    let mut runtime = Runtime::from_fixture(fixture(true, true).await).await;
    let original = episode(&runtime).await;
    assert!(original.source_read.as_ref().unwrap().needs_admission());
    let (capture, answered) = complete(&mut runtime).await;
    let SourceReadStageState::Answered { answer } = &answered.source_read.as_ref().unwrap().state else { panic!() };
    let SourceReadAnswer::TemporaryResources(answer) = answer.as_ref() else { panic!("temporary resource answer") };
    assert_eq!(answer.basis, original.first);
    assert_eq!(answer.result, capture);
    assert_eq!(answer.observed_tasks, 2);
    assert_eq!(answer.checked_task_context_count, 2);
    assert!(answer.contributions.iter().any(|entry| matches!(
        entry,
        TemporaryResourceContribution::Observed { mechanism: TemporaryResourceMechanism::UserAllocations, .. }
    )));
    assert!(answer.contributions.iter().any(|entry| matches!(
        entry,
        TemporaryResourceContribution::Unresolved { mechanism: TemporaryResourceMechanism::SpillOrEstimate }
    )));
    assert_eq!(answered.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(answered.first, original.first);
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    runtime.reconcile().await;
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), [2; 5]);
}
#[tokio::test]
async fn unqualified_and_healthy_originals_open_no_competing_source_and_keep_explicit_status() {
    let mut unqualified = Runtime::from_fixture(fixture(false, true).await).await;
    let original = episode(&unqualified).await;
    assert!(matches!(
        original.source_read.as_ref().unwrap().state,
        SourceReadStageState::Deferred { issue: DiagnosticAnalysisIssue::CostUnqualified }
    ));
    assert!(!unqualified.enroll().await);
    assert_eq!(unqualified.native_calls(), [1; 5]);
    let mut healthy = Runtime::from_fixture(fixture(true, false).await).await;
    assert!(episodes(&healthy).await.is_empty());
    assert!(!healthy.enroll().await);
    assert_eq!(healthy.native_calls(), [1; 5]);
}
#[tokio::test]
async fn partial_native_allocations_answer_only_context_and_preserve_missing_competing_mechanisms() {
    let mut runtime = Runtime::from_fixture(fixture(true, true).await).await;
    let original = episode(&runtime).await;
    let focus = &original.source_read.as_ref().unwrap().focus;
    let execution = SourceReadExecutionScope::from_context(
        focus.question,
        &NativeReadContext::from_snapshot(&runtime.fixture.backend.activity.source).unwrap(),
    )
    .unwrap();
    let (source, records) = temporary_resource_result(focus, &execution, culprit_core::SourceState::Partial);
    runtime.fixture.backend.source_read_result = Some(Frame { source, records });
    let (_, answered) = complete(&mut runtime).await;
    let SourceReadStageState::Answered { answer } = &answered.source_read.as_ref().unwrap().state else { panic!() };
    let SourceReadAnswer::TemporaryResources(answer) = answer.as_ref() else { panic!("temporary resource answer") };
    assert_eq!(answer.source_state, culprit_core::SourceState::Partial);
    assert_eq!(answer.observed_tasks, 5);
    assert_eq!(answer.checked_task_context_count, 2);
    assert_eq!(answered.lifecycle, DiagnosticLifecycle::Active);
}
#[tokio::test]
async fn source_failure_and_cancellation_keep_attempt_original_and_native_retirement_without_a_cause() {
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let mut runtime = Runtime::from_fixture(fixture(true, true).await).await;
        let original = episode(&runtime).await;
        assert!(runtime.enroll().await);
        let job = runtime.take_focus().await;
        runtime.fixture.backend.focus_failure = Some(stop);
        let result = runtime.try_execute(&job).await;
        if stop == CollectionStop::CancellationUnconfirmed {
            assert_eq!(CollectionStop::from_error(&result.err().unwrap()), stop);
            assert_eq!(runtime.fixture.outbox.status(100, None).await.unwrap().unconfirmed_sources, 1);
            assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().contains(&job.key));
        } else {
            let result = result.unwrap();
            assert_ne!(result.event.status, CollectionStatus::Complete);
            runtime.finish(result.outcome).await;
        }
        let stopped = episode(&runtime).await;
        assert_eq!(stopped.first, original.first);
        assert!(matches!(stopped.source_read.as_ref().unwrap().state, SourceReadStageState::Stopped { .. }));
        assert!(stopped.source_read_capture().is_none());
        assert_eq!(runtime.native_calls()[3], 1, "failed source is not verified after failure");
        assert_eq!(runtime.native_calls()[4], 2, "same admitted reader retains close ownership");
        if stop != CollectionStop::CancellationUnconfirmed {
            assert!(!runtime.enroll().await);
        }
    }
}
#[tokio::test]
async fn qualified_recovery_stops_new_question_but_preserves_prior_answer_and_new_original_receipts() {
    let mut pending = Runtime::from_fixture(fixture(true, true).await).await;
    let before = episode(&pending).await;
    recover(&mut pending).await;
    let recovered = latest_history_episode(&pending, &before.id).await;
    assert_eq!(recovered.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(recovered.first, before.first);
    assert_eq!(recovered.source_read.as_ref().unwrap().state, SourceReadStageState::Recovered);
    assert!(episodes(&pending).await.is_empty(), "terminal cleanup work leaves the live working set");
    assert!(!pending.enroll().await);
    assert_eq!(pending.native_calls(), [1; 5], "offline recovery never opens the competing source");

    let mut answered = Runtime::from_fixture(fixture(true, true).await).await;
    let (_, before) = complete(&mut answered).await;
    recover(&mut answered).await;
    let recovered = latest_history_episode(&answered, &before.id).await;
    assert_eq!(recovered.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(recovered.source_read, before.source_read);
    assert_eq!(recovered.first, before.first);
    assert!(episodes(&answered).await.is_empty());
    assert!(!answered.enroll().await);
    assert_eq!(answered.native_calls(), [2; 5]);
}

#[tokio::test]
async fn ordinary_or_unknown_age_boundaries_remain_visible_without_taskspace_enrollment_or_native_open() {
    for age in [None, Some("0"), Some("1799")] {
        let mut runtime = Runtime::from_fixture(fixture_with_age(true, true, age).await).await;
        let original = episode(&runtime).await;
        assert!(matches!(original.report, Some(DiagnosticReport::OriginalObservation { .. })));
        assert_eq!(original.lifecycle, DiagnosticLifecycle::Active);
        assert!(original.source_read.is_none(), "{age:?}");
        assert!(!runtime.enroll().await);
        assert_eq!(runtime.native_calls(), [1; 5], "cost-qualified healthy context opens no follow-up");
    }
}

#[tokio::test]
async fn the_existing_attention_boundary_enrolls_only_with_its_exact_native_original_and_cost_gate() {
    let mut runtime = Runtime::from_fixture(fixture_with_age(true, true, Some("1800")).await).await;
    let original = episode(&runtime).await;
    let stage = original.source_read.as_ref().unwrap();
    assert_eq!(*stage.focus.basis, original.first);
    assert_ne!(*stage.focus.basis, original.latest, "a later baseline cannot replace the first adverse receipt");
    assert!(matches!(stage.focus.prerequisite, SourceReadPrerequisite::ObservedCleanupBoundary { .. }));
    let (_, answered) = complete(&mut runtime).await;
    assert!(matches!(answered.source_read.as_ref().unwrap().state, SourceReadStageState::Answered { .. }));
    assert_eq!(runtime.native_calls(), [2; 5]);
}
