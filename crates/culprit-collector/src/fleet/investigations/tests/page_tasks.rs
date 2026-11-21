//! Existing scheduler/Outbox owners run sealed synthetic native facts. These are not native cost approvals.
use super::{
    runtime::Runtime,
    support::{Frame, Harness, capture_frame},
};
use crate::{
    analytical::test_support::{PageTaskHolderCase, page_task_holder_fixture},
    capture::CollectionCancellation,
    diagnostics::analyze_pending,
    fleet::{journal::FocusedStage, scheduler::Outcome},
    outbox::{DiagnosticInputState, DiagnosticUpdate, Outbox},
};
use culprit_core::telemetry::*;
use std::sync::atomic::Ordering;
mod recovery;

fn activity(long_running: bool, siblings: bool) -> Frame {
    let fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    let mut records = fixture.activity_rows;
    for (index, row) in records.iter_mut().enumerate() {
        let TelemetryRecord::Activity(row) = row else { panic!() };
        row.elapsed_ms = Some(if long_running && (index == 0 || siblings) { "120000" } else { "1" }.into());
    }
    Frame { source: fixture.activity_source, records }
}

async fn fixture(long_running: bool, siblings: bool) -> Harness {
    let mut fixture = Harness::with_activity(activity(long_running, siblings)).await;
    configure_recipe(&mut fixture);
    fixture
}

fn configure_recipe(fixture: &mut Harness) {
    fixture.backend.recipe.id = "fixture.paired-page-tasks".into();
    fixture.backend.recipe.selector = RecipeSelector::PageWaitTasks;
    let RecipeCostQualification::Measured(cost) = &mut fixture.backend.recipe.cost else { panic!() };
    cost.recipe_id = fixture.backend.recipe.id.clone();
    cost.selector = RecipeSelector::PageWaitTasks;
    assert!(fixture.backend.recipe.is_valid(), "mock accounting is never production qualification");
}

pub(super) fn population_frame(
    activity: &DiagnosticCaptureRef,
    original: &NativeReadContext,
    current: &Frame,
) -> Frame {
    let fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    let mut source = fixture.page_source;
    let mut records = fixture.page_rows;
    let native = current.source.observation.activity().unwrap();
    let from = chrono::DateTime::parse_from_rfc3339(&native.observed_until).unwrap() + chrono::Duration::seconds(1);
    let at = |milliseconds| (from + chrono::Duration::milliseconds(milliseconds)).to_rfc3339();
    let SourceObservation::PageWaitTargets { snapshot, database_writable, original_activity } = &mut source.observation
    else {
        panic!()
    };
    snapshot.observed_from = at(0);
    snapshot.observed_until = at(500);
    *original_activity = Some(activity.clone());
    *database_writable = original.database_writable;
    source.target = current.source.target.clone();
    source.partition = current.source.partition.clone();
    let TelemetryRecord::Operational(row) = &mut records[0] else { panic!() };
    let OperationalFact::PageWait(point) = &mut row.fact else { panic!() };
    point.observed_from = at(0);
    point.observed_until = at(500);
    point.before_check.as_mut().unwrap().observed_at = at(100);
    point.after_check.as_mut().unwrap().observed_at = at(400);
    point.task_wait.as_mut().unwrap().observed_at = at(0);
    row.key = point.key().unwrap();
    assert!(point.fits_source_scope(&source));
    assert!(NativeReadContext::from_snapshot(&source).unwrap().matches_earlier_scope(original));
    Frame { source, records }
}

async fn intent(runtime: &Runtime) -> DiagnosticInvestigationIntent {
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    let [intent] = intents.as_slice() else { panic!("one population uses the existing queue") };
    intent.clone()
}

#[tokio::test]
async fn incident_uses_its_first_authenticated_frame_and_existing_queue_then_retains_independent_task_history() {
    let mut runtime = Runtime::from_fixture(fixture(true, false).await).await;
    let checkpoints =
        runtime.fixture.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    let original = checkpoints
        .checkpoints
        .iter()
        .flat_map(|checkpoint| &checkpoint.episodes)
        .find(|episode| {
            matches!(episode.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
        })
        .unwrap()
        .clone();
    assert_ne!(original.first, original.latest, "runtime captured a later baseline, without replacing the first frame");
    assert!(runtime.enroll().await);
    assert!(!runtime.enroll().await);
    let intent = intent(&runtime).await;
    let DiagnosticFocus::PageWaitTasks { focus, original_context, .. } = &intent.focus else { panic!() };
    assert_eq!(focus.parent, original.first);
    assert_ne!(focus.parent, original.latest);
    let preserved = runtime.fixture.outbox.diagnostic_evidence(&focus.parent).await.unwrap();
    let original_input = runtime
        .fixture
        .outbox
        .diagnostic_input(&focus.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(**original_context, original_input.activity_frame().unwrap().context());
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Complete);
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    let manifest = runtime.fixture.outbox.verify(&capture.capture.observation_id).await.unwrap();
    assert_eq!(manifest.investigation_intent().unwrap(), Some(intent.clone()));
    runtime.finish(result.outcome).await;
    let calls = runtime.native_calls();
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(runtime.native_calls(), calls, "offline interpretation performs no source work");
    let revisions = runtime.revisions(&capture.capture).await;
    let trigger = revisions.iter().find(|revision| revision.episode.id == intent.episode_id).unwrap();
    assert_eq!(trigger.episode.lifecycle, original.lifecycle);
    assert_eq!(trigger.episode.recovery, original.recovery);
    assert_eq!(trigger.episode.analysis, DiagnosticAnalysisStatus::Partial);
    assert!(trigger.inputs.contains(&original.first));
    let task = revisions
        .iter()
        .find(|revision| matches!(revision.episode.signal, DiagnosticSignal::PageTaskWait { .. }))
        .unwrap();
    assert_eq!(task.episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert!(task.episode.recovery.is_none());
    assert!(intent.permits_result_episode(&task.episode));
    assert_eq!(runtime.fixture.outbox.diagnostic_evidence(&original.first).await.unwrap(), preserved);
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    runtime.reconcile().await;
    assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
    assert!(matches!(
        runtime.fixture.journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));
    assert!(!runtime.enroll().await, "unresolved analysis is not independent population polling");
    assert_eq!(runtime.native_calls(), calls);
}

#[tokio::test]
async fn a_healthy_frame_or_unqualified_population_never_enrolls_or_opens_an_investigation() {
    for healthy in [true, false] {
        let mut fixture = fixture(!healthy, false).await;
        if !healthy {
            fixture.backend.recipe.cost = RecipeCostQualification::Unqualified;
        }
        let mut runtime = Runtime::from_fixture(fixture).await;
        let before = runtime.native_calls();
        assert!(!runtime.enroll().await);
        assert_eq!(runtime.native_calls(), before);
        assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 0);
        assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_declared_but_unqualified_request_reader_is_not_bypassed_by_a_measured_population() {
    let mut fixture = fixture(true, false).await;
    let mut request = fixture.backend.recipe.clone();
    request.id = "fixture.current-request-work".into();
    request.selector = RecipeSelector::CurrentRequestWork;
    request.cost = RecipeCostQualification::Unqualified;
    fixture.backend.additional_recipes.push(request);
    let mut runtime = Runtime::from_fixture(fixture).await;
    let before = runtime.native_calls();
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), before);
    assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn several_incident_requests_from_the_same_first_capture_share_one_population_stage() {
    let mut runtime = Runtime::from_fixture(fixture(true, true).await).await;
    assert!(runtime.enroll().await);
    assert_eq!(runtime.fixture.journal.focused_intents().await.unwrap().len(), 1);
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), [1; 5]);
    assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_empty_partial_population_keeps_its_header_original_and_unresolved_incident_without_fake_recovery() {
    let mut runtime = Runtime::from_fixture(fixture(true, false).await).await;
    assert!(runtime.enroll().await);
    let intent = intent(&runtime).await;
    let DiagnosticFocus::PageWaitTasks { focus, original_context, .. } = &intent.focus else { panic!() };
    let mut empty = population_frame(&focus.parent.capture_ref(), original_context, &runtime.fixture.backend.activity);
    empty.records.clear();
    let SourceObservation::PageWaitTargets { snapshot, .. } = &mut empty.source.observation else { panic!() };
    snapshot.records = 0;
    snapshot.sources[0].records = 0;
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    runtime.fixture.backend.page_tasks = Some(empty);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
        .await
        .unwrap();
    let revisions = runtime.revisions(&capture.capture).await;
    let [revision] = revisions.as_slice() else { panic!("only the original trigger changed its analysis status") };
    assert_eq!(revision.episode.id, intent.episode_id);
    assert_eq!(revision.episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(revision.episode.issue, Some(DiagnosticAnalysisIssue::EvidenceUnavailable));
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Active);
    assert!(revision.episode.recovery.is_none());
    assert!(revision.inputs.contains(&focus.parent));
    runtime.reconcile().await;
    assert!(!runtime.enroll().await);
}

#[tokio::test]
async fn changed_route_refuses_before_open_and_failed_native_describe_keeps_the_admitted_parent() {
    for changed_route in [true, false] {
        let mut runtime = Runtime::from_fixture(fixture(true, false).await).await;
        assert!(runtime.enroll().await);
        let intent = intent(&runtime).await;
        let job = runtime.take_focus().await;
        let before = runtime.native_calls();
        if changed_route {
            runtime.fixture.backend.changed.store(true, Ordering::SeqCst);
            let result = runtime.execute(&job).await;
            assert_eq!(runtime.native_calls(), before);
            runtime.finish(result.outcome).await;
        } else {
            runtime.fixture.backend.focus_failure = Some(CollectionStop::SourceUnavailable);
            let result = runtime.execute(&job).await;
            assert!(matches!(result.event.status, CollectionStatus::Incomplete(CollectionStop::SourceUnavailable)));
            let capture = result.event.capture.clone().unwrap();
            let owner = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
            let receipt = runtime.fixture.outbox.attempt_receipt(&capture, Some(&owner)).await.unwrap();
            let bytes = std::fs::read(
                runtime
                    .fixture
                    ._directory
                    .path()
                    .join("outbox/captures")
                    .join(&capture.observation_id)
                    .join("manifest.json"),
            )
            .unwrap();
            assert!(bytes.len() <= crate::collection_files::MAX_MANIFEST_BYTES);
            assert_eq!(receipt.manifest_sha256, Some(digest(&bytes)));
            let manifest: CollectionManifest = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::SourceUnavailable));
            assert!(manifest.description.is_none());
            assert_eq!(manifest.investigation_intent().unwrap(), Some(intent.clone()));
            assert!(runtime.fixture.outbox.diagnostic_evidence(intent.focus.parent()).await.is_ok());
            assert_eq!(runtime.native_calls()[4], before[4] + 1, "the same native observer is closed");
            runtime.finish(result.outcome).await;
        }
    }
}

#[tokio::test]
async fn pending_question_reopens_with_its_exact_first_frame_context_and_pin_without_dispatch() {
    let mut runtime = Runtime::from_fixture(fixture(true, false).await).await;
    assert!(runtime.enroll().await);
    let intent = intent(&runtime).await;
    let calls = runtime.native_calls();
    let Harness { _directory, inventory, outbox, journal, bindings, backend } = runtime.fixture;
    let path = _directory.path().join("outbox");
    let journal_path = _directory.path().join("fleet");
    drop(outbox);
    drop(journal);
    let mut outbox = Outbox::open(&path).unwrap();
    outbox.recover().await.unwrap();
    let journal = crate::fleet::FleetJournal::open(&journal_path, &inventory, "one").await.unwrap();
    assert_eq!(journal.focused_intents().await.unwrap(), vec![intent.clone()]);
    assert!(outbox.investigation_parent_keys().await.unwrap().contains(&intent.key().unwrap()));
    let original = outbox
        .diagnostic_input(&intent.focus.parent().capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    let DiagnosticFocus::PageWaitTasks { original_context, .. } = &intent.focus else { panic!() };
    assert_eq!(**original_context, original.activity_frame().unwrap().context());
    let mut restarted = Harness { _directory, inventory, outbox, journal, bindings, backend };
    assert!(
        restarted.prepare(&CollectionCancellation::default()).await.unwrap().is_empty(),
        "the canonical outstanding stage deduplicates after restart"
    );
    assert_eq!(restarted.backend.calls.opened.load(Ordering::SeqCst), calls[1]);
}

#[tokio::test]
async fn missing_independent_cost_evidence_withholds_the_population_without_opening_a_source() {
    let mut fixture = fixture(true, false).await;
    let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.recipe.cost else { panic!() };
    receipt.evidence.capture.observation_id = "unavailable-cost-capture".into();
    receipt.evidence.batch_id = batch_id(&receipt.evidence.capture.observation_id, receipt.evidence.stream).unwrap();
    assert!(fixture.backend.recipe.is_valid());
    let mut runtime = Runtime::from_fixture(fixture).await;
    let before = runtime.native_calls();
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), before);
    assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 0);
    assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_before_native_admission_keeps_the_pending_question_and_original_pin() {
    let mut runtime = Runtime::from_fixture(fixture(true, false).await).await;
    assert!(runtime.enroll().await);
    let intent = intent(&runtime).await;
    let job = runtime.take_focus().await;
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    let admitted = crate::fleet::admission::AdmittedBackend::new(&runtime.fixture.backend, tokio::time::Instant::now());
    let before = runtime.native_calls();
    let result = runtime
        .investigations
        .execute(
            &runtime.fixture.backend,
            &admitted,
            &runtime.fixture.journal,
            &mut crate::fleet::storage::CaptureStorage::local(&mut runtime.fixture.outbox),
            &job,
            &cancellation,
        )
        .await
        .unwrap();
    assert_eq!(result.event.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
    assert_eq!(runtime.native_calls(), before);
    assert!(result.event.capture.is_none());
    runtime.finish(result.outcome).await;
    assert_eq!(runtime.fixture.journal.focused_intents().await.unwrap(), vec![intent.clone()]);
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().contains(&intent.key().unwrap()));
    assert!(runtime.fixture.outbox.diagnostic_evidence(intent.focus.parent()).await.is_ok());
}

#[tokio::test]
async fn an_original_missing_token_cannot_starve_an_eligible_sibling_from_the_same_frame() {
    let mut original = activity(true, true);
    let TelemetryRecord::Activity(invalid) = &mut original.records[0] else { panic!() };
    invalid.statement_identity = None;
    let mut fixture = Harness::with_activity(original).await;
    configure_recipe(&mut fixture);
    let mut runtime = Runtime::from_fixture(fixture).await;
    // A checkpoint is an unordered set of episodes. Persist a valid ordering with the ineligible trigger
    // first, using the real observer and writer; no receipt, native row or authority is altered.
    let native = runtime.fixture.backend.activity.clone();
    capture_frame(&mut runtime.fixture.outbox, native).await;
    let input = runtime.fixture.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap();
    let rows = input
        .rows
        .iter()
        .map(|row| {
            let TelemetryRecord::Activity(observation) = &row.row.record else { panic!() };
            DiagnosticActivityRow { reference: &row.reference, observation }
        })
        .collect::<Vec<_>>();
    let at = chrono::Utc::now().to_rfc3339();
    let observed = observe_diagnostic_activity(
        &input.source,
        &input.parent,
        &rows,
        input.checkpoint.as_ref(),
        &ActivityPolicy::default(),
        &at,
    )
    .unwrap();
    let invalid = rows.iter().find(|row| row.observation.statement_identity.is_none()).unwrap().observation.key.clone();
    let mut update = DiagnosticUpdate {
        step_id: digest(&serde_json::to_vec(&("invalid-population-trigger-first", &input.parent)).unwrap()),
        analyzed_at: at,
        revisions: observed.revisions,
        checkpoint: observed.checkpoint,
    };
    update.checkpoint.episodes.sort_by_key(|episode| episode.signal.activity_key() != Some(&invalid));
    assert_eq!(update.checkpoint.episodes[0].signal.activity_key(), Some(&invalid));
    runtime.fixture.outbox.commit_diagnostics(input, update).await.unwrap();
    let before = runtime.native_calls();
    assert!(runtime.enroll().await);
    let admitted = intent(&runtime).await;
    assert_ne!(admitted.focus.activity_participant().unwrap().original.key, invalid);
    assert!(admitted.focus.activity_participant().unwrap().original.statement_identity.is_some());
    assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.native_calls(), before, "invalid triggers do not open native work");
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 1);
}
