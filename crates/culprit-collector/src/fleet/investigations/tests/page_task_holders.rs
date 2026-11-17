//! Actual sealed original captures drive synthetic admission tests; no fixture qualifies production native cost.
use super::{
    runtime::Runtime,
    support::{Frame, Harness, capture_frame},
};
use crate::{
    analytical::test_support::{PageTaskHolderCase, page_task_holder_fixture},
    capture::CollectionCancellation,
    diagnostics::{DiagnosticPolicies, drain_pending},
    fleet::scheduler::Outcome,
};
use culprit_core::telemetry::*;

pub(in crate::fleet::investigations) fn focused_frame(focus: &PageTaskHolderFocus) -> Frame {
    let fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    let mut source = fixture.source;
    let mut records = fixture.records;
    let SourceObservation::PageTaskHolders { focus: declared, .. } = &mut source.observation else { panic!() };
    **declared = focus.clone();
    let TelemetryRecord::Operational(row) = &mut records[0] else { panic!() };
    let OperationalFact::PageTaskHolders(point) = &mut row.fact else { panic!() };
    point.focus_key = focus.key().unwrap();
    row.key = point.key().unwrap();
    assert!(focus.accepts_observation(&NativeReadContext::from_snapshot(&source).unwrap(), point));
    Frame { source, records }
}

pub(in crate::fleet::investigations) async fn paired_harness() -> Harness {
    let fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    let mut harness =
        Harness::with_activity(Frame { source: fixture.activity_source, records: fixture.activity_rows }).await;
    let original = harness
        .outbox
        .status(50, None)
        .await
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| matches!(entry.request.selection, CollectionSelection::Activity))
        .unwrap()
        .capture;
    let original =
        harness.outbox.diagnostic_input(&original.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    let mut page_source = fixture.page_source;
    let SourceObservation::PageWaitTargets { original_activity, .. } = &mut page_source.observation else { panic!() };
    *original_activity = Some(original.parent.clone());
    capture_frame(&mut harness.outbox, Frame { source: page_source, records: fixture.page_rows }).await;
    let policies =
        DiagnosticPolicies::for_activity("saved-source", CollectionSubject::Database, ActivityPolicy::default())
            .unwrap();
    drain_pending(&mut harness.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    let episode = task_episode(&harness).await;
    let recipe = &mut harness.backend.recipe;
    recipe.id = "fixture.task-holders".into();
    recipe.selector = RecipeSelector::FocusedPageTaskHolders;
    let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
    receipt.recipe_id = recipe.id.clone();
    receipt.selector = recipe.selector;
    receipt.scope = RecipeCostScope::Activity(episode.scope.clone());
    assert!(recipe.is_valid(), "the mock verifier is structural, never native cost qualification");
    let native = &mut harness.backend.activity.source;
    let SourceObservation::Activity(frame) = &mut native.observation else { panic!() };
    frame.at = "2020-01-01T12:00:02Z".into();
    frame.observed_from = frame.at.clone();
    frame.observed_until = frame.at.clone();
    // The fresh route observation is independently captured; it never replaces the original actor frame.
    capture_frame(&mut harness.outbox, harness.backend.activity.clone()).await;
    drain_pending(&mut harness.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    harness.bindings.observe("target", &harness.backend.activity.source, &harness.backend.revision).unwrap();
    harness
}

pub(in crate::fleet::investigations) async fn task_episode(harness: &Harness) -> DiagnosticEpisode {
    harness
        .outbox
        .diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10)
        .await
        .unwrap()
        .checkpoints
        .into_iter()
        .flat_map(|checkpoint| checkpoint.episodes)
        .find(|episode| matches!(episode.signal, DiagnosticSignal::PageTaskWait { .. }))
        .unwrap()
}

#[tokio::test]
async fn exact_task_question_uses_existing_one_outstanding_queue_and_its_own_native_result_history() {
    let fixture = paired_harness().await;
    let original = task_episode(&fixture).await;
    let mut runtime = Runtime::from_fixture(fixture).await;
    assert!(runtime.enroll().await);
    assert!(!runtime.enroll().await, "one task point has one outstanding question");
    let job = runtime.take_focus().await;
    let intent = runtime.fixture.journal.focused_intent(&job.key).await.unwrap().unwrap();
    let DiagnosticFocus::PageTaskHolders { focus } = &intent.focus else { panic!() };
    assert!(original.matches_page_task_holder_focus(focus));
    let before = runtime.native_calls();
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: captured } = &result.outcome else { panic!("one owned native aggregate") };
    let captured = captured.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    let history = runtime.revisions(&captured.capture).await;
    let revision = history.iter().find(|revision| revision.episode.id == original.id).unwrap();
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert!(revision.episode.recovery.is_none());
    assert!(matches!(revision.episode.report, Some(DiagnosticReport::PageTaskHolders { .. })));
    assert!(revision.inputs.contains(&focus.page));
    assert!(revision.inputs.contains(&focus.participant.parent));
    assert_eq!(runtime.native_calls(), before.map(|count| count + 1));
    runtime.reconcile().await;
    assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
}
