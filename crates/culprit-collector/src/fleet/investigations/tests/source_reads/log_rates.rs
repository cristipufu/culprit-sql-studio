//! Controlled facts exercise the real outbox, scheduler and admission path; no fixture approves native cost.
use super::*;
use crate::analytical::test_support::hyperscale_log_component_evidence_cases;

pub(in crate::fleet::investigations) fn frame(focus: &SourceReadFocus, execution: &NativeSourceReadScope) -> Frame {
    let (mut source, records) = hyperscale_log_component_evidence_cases().remove(0);
    source.target = focus.target.clone();
    source.partition = SourcePartition::Target;
    let snapshot = source.observation.snapshot().unwrap().clone();
    source.observation = SourceObservation::SourceRead {
        execution: Box::new(NativeReadContext {
            target: focus.target.clone(),
            partition: execution.partition.clone(),
            database_writable: Some(execution.database_writable),
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
        }),
        focus_key: focus.key().unwrap(),
        snapshot,
    };
    Frame { source, records }
}

async fn fixture(cost_qualified: bool) -> Harness {
    let compiler = compiler_admission_investigation_fixture();
    let mut source = compiler.baseline_source;
    // Bind the original incident to the controlled component namespace before either capture is sealed.
    source.target = hyperscale_log_component_evidence_cases().remove(0).0.target;
    let mut row = compiler.baseline_record.record;
    let TelemetryRecord::Activity(actor) = &mut row else { panic!() };
    actor.database = source.target.database().map(|database| database.database.clone());
    let wait = actor.wait.as_mut().unwrap();
    wait.class = Some(ActivityWaitClass::LogRateGovernance);
    wait.native_type = Some("LOG_RATE_GOVERNOR".into());
    let mut harness = Harness::with_activity(Frame { source, records: vec![row] }).await;
    harness.backend.recipe.id = "fixture.hyperscale-log-components".into();
    harness.backend.recipe.selector = SourceReadQuestion::HyperscaleLogComponents.selector();
    let RecipeCostQualification::Measured(receipt) = &mut harness.backend.recipe.cost else { panic!() };
    receipt.recipe_id = harness.backend.recipe.id.clone();
    receipt.selector = harness.backend.recipe.selector;
    receipt.tested_budget.max_snapshot_rows = 64;
    receipt.tested_budget.page_rows = 64;
    receipt.scope = RecipeCostScope::NativeRead(
        NativeSourceReadScope::from_context(
            &NativeReadContext::from_activity(&harness.backend.activity.source).unwrap(),
        )
        .unwrap()
        .into(),
    );
    if !cost_qualified {
        harness.backend.recipe.cost = RecipeCostQualification::Unqualified;
    }
    harness
}

async fn log_episode(runtime: &mut Runtime) -> DiagnosticEpisode {
    runtime
        .fixture
        .outbox
        .diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 8)
        .await
        .unwrap()
        .checkpoints
        .into_iter()
        .flat_map(|checkpoint| checkpoint.episodes)
        .find(|episode| matches!(episode.signal, DiagnosticSignal::LogRateGovernance { .. }))
        .unwrap()
}

#[tokio::test]
async fn unqualified_log_component_question_keeps_original_incident_without_opening_source() {
    let mut runtime = Runtime::from_fixture(fixture(false).await).await;
    let incident = log_episode(&mut runtime).await;
    assert!(incident.report.is_none());
    assert!(matches!(
        incident.source_read.as_ref().unwrap().state,
        SourceReadStageState::Deferred { issue: DiagnosticAnalysisIssue::CostUnqualified }
    ));
    let before = runtime.native_calls();
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), before);
    assert_eq!(runtime.fixture.backend.components.load(Ordering::SeqCst), 0);
    assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
    assert_eq!(log_episode(&mut runtime).await.first, incident.first);
}

#[tokio::test]
async fn native_log_wait_routes_directly_to_one_authenticated_component_capture_without_a_compiler_read() {
    let mut runtime = Runtime::from_fixture(fixture(true).await).await;
    let original = log_episode(&mut runtime).await;
    assert!(runtime.enroll().await);
    let intent = runtime.fixture.journal.focused_intents().await.unwrap().remove(0);
    let DiagnosticFocus::SourceRead { focus } = &intent.focus else { panic!() };
    assert_eq!(focus.question, SourceReadQuestion::HyperscaleLogComponents);
    assert_eq!(focus.prerequisite, SourceReadPrerequisite::ObservedLogRateWait);
    assert_eq!(intent.focus.references().collect::<Vec<_>>(), [&original.first]);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!("{:?}", result.event) };
    runtime.finish(Outcome::FocusedComplete { result: capture.clone() }).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let answered = log_episode(&mut runtime).await;
    assert_eq!(answered.first, original.first);
    assert!(answered.report.is_none(), "affected query and component limits are not a fabricated root cause");
    assert_eq!(answered.source_read_capture(), Some(&capture));
    runtime.reconcile().await;
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), [2; 5], "one baseline and one component read, no compiler prerequisite");
}

#[tokio::test]
async fn a_resolved_log_incident_still_owes_its_one_original_component_question() {
    let mut runtime = Runtime::from_fixture(fixture(true).await).await;
    let original = log_episode(&mut runtime).await;
    runtime.complete_empty_baseline().await;
    let recovered = log_episode(&mut runtime).await;
    assert_eq!(recovered.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(recovered.first, original.first);
    assert!(recovered.has_owed_question());
    assert!(runtime.enroll().await);
    let intent = runtime.fixture.journal.focused_intents().await.unwrap().remove(0);
    assert_eq!(intent.focus.parent(), &original.first);
    assert_eq!(runtime.native_calls(), [2; 5], "planning reads original sealed facts without native work");
}

#[tokio::test]
async fn refused_source_cost_or_changed_route_cannot_open_the_log_component_reader() {
    for changed_route in [true, false] {
        let mut runtime = Runtime::from_fixture(fixture(true).await).await;
        if changed_route {
            assert!(runtime.enroll().await);
            let job = runtime.take_focus().await;
            runtime.fixture.backend.changed.store(true, Ordering::SeqCst);
            let result = runtime.execute(&job).await;
            assert!(result.event.capture.is_none());
        } else {
            runtime.fixture.backend.accept = false;
            assert!(!runtime.enroll().await);
        }
        assert_eq!(runtime.native_calls(), [1; 5]);
    }
}
