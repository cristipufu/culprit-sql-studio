//! Real durable admission and offline consumption over synthetic compiler evidence; never production qualification.
use super::{
    runtime::Runtime,
    support::{Frame, Harness},
};
use crate::{
    analytical::test_support::{compiler_admission_evidence, compiler_admission_investigation_fixture},
    capture::CollectionCancellation,
    diagnostics::analyze_pending,
    fleet::{journal::FocusedStage, scheduler::Outcome},
    outbox::DiagnosticInputState,
};
use culprit_core::telemetry::*;

pub(super) fn focused_frame(focus: &ActivityRequestFocus) -> Frame {
    let (_, mut records) = compiler_admission_evidence();
    let at = (chrono::DateTime::parse_from_rfc3339(&focus.original_observed_at).unwrap()
        + chrono::Duration::seconds(1))
    .to_rfc3339();
    let mut gateways = Vec::new();
    for record in &mut records {
        let TelemetryRecord::Operational(row) = record else { panic!() };
        match &mut row.fact {
            OperationalFact::CompilerWaiter(waiter) => {
                waiter.request = focus.original.clone();
                waiter.observed_at = at.clone();
                row.key = waiter.key().unwrap();
            }
            OperationalFact::CounterSet(counters) => gateways.push(counters.identity.clone()),
            _ => panic!(),
        }
    }
    Frame {
        source: CollectionDescription {
            target: focus.target.clone(),
            partition: focus.partition.clone(),
            source: culprit_core::source_id::COMPILATION_GATEWAYS.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedCompilationAdmission {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::CompilerAdmission,
                    observed_from: at.clone(),
                    observed_until: at.clone(),
                    records: records.len() as u32,
                    sources: vec![StateSourceCoverage::collected(
                        culprit_core::source_id::COMPILATION_GATEWAYS.into(),
                        records.len() as u32,
                    )],
                },
                result: Box::new(ActivityCompilationAdmissionFocusResult {
                    focus: focus.clone(),
                    observed_from: at.clone(),
                    observed_until: at,
                    source: FocusSourceState::Matched,
                    membership: CompilationAdmissionMembership::Observed,
                    gateways,
                }),
            },
        },
        records,
    }
}

async fn fixture() -> Harness {
    let mut fixture = Harness::new(1).await;
    let compiler = compiler_admission_investigation_fixture();
    fixture.backend.activity =
        Frame { source: compiler.baseline_source, records: vec![compiler.baseline_record.record] };
    fixture.backend.recipe.selector = RecipeSelector::FocusedCompilationAdmission;
    fixture.backend.recipe.id = "synthetic.focused-compiler-admission".into();
    let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.recipe.cost else { panic!() };
    receipt.selector = RecipeSelector::FocusedCompilationAdmission;
    receipt.recipe_id = fixture.backend.recipe.id.clone();
    receipt.scope = DiagnosticScope::from_source(&fixture.backend.activity.source).unwrap().into();
    receipt.tested_budget.max_snapshot_rows = 64;
    receipt.tested_budget.page_rows = 64;
    fixture
}

#[tokio::test]
async fn an_admitted_compiler_waiter_retains_original_incident_and_all_gateway_receipts_after_consumption() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    assert!(runtime.enroll().await);
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    let [intent] = intents.as_slice() else { panic!() };
    let intent = intent.clone();
    assert_eq!(intent.focus.selector(), RecipeSelector::FocusedCompilationAdmission);
    let parent = intent.focus.participant().parent.clone();
    let original = runtime.fixture.outbox.diagnostic_evidence(&parent).await.unwrap();
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Complete);
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
    runtime.finish(Outcome::FocusedComplete { result: capture.clone() }).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let revisions = runtime.revisions(&capture.capture).await;
    let revision = revisions.iter().find(|revision| revision.episode.id == intent.episode_id).unwrap();
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(revision.episode.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert!(revision.episode.recovery.is_none());
    assert_eq!(revision.episode.latest, parent);
    let Some(DiagnosticReport::CompilationAdmission { observation, gateways }) = &revision.episode.report else {
        panic!()
    };
    assert!(!gateways.is_empty());
    assert!(revision.inputs.contains(observation));
    assert!(gateways.iter().all(|reference| revision.inputs.contains(reference)));
    assert!(revision.inputs.contains(&parent));
    assert_eq!(runtime.fixture.outbox.diagnostic_evidence(&parent).await.unwrap(), original);
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    runtime.reconcile().await;
    assert!(matches!(
        runtime.fixture.journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));
    assert!(!runtime.enroll().await, "the same original compiler incident must not spin completed questions");
    assert_eq!(runtime.native_calls(), [2; 5]);
}

#[tokio::test]
async fn unqualified_cost_never_dispatches_the_compiler_followup() {
    let mut fixture = fixture().await;
    fixture.backend.recipe.cost = RecipeCostQualification::Unqualified;
    let mut runtime = Runtime::from_fixture(fixture).await;
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), [1; 5]);
    let checkpoints =
        runtime.fixture.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 8).await.unwrap();
    assert!(checkpoints.checkpoints.iter().flat_map(|checkpoint| &checkpoint.episodes).any(|episode| matches!(
        episode.signal,
        DiagnosticSignal::CompilationAdmission { .. }
    ) && episode.issue
        == Some(DiagnosticAnalysisIssue::CostUnqualified)));
}
