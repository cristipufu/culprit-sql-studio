//! Real Outbox/Fleet/CAS ownership over controlled native facts; no fixture qualifies production cost.
use super::{
    runtime::Runtime,
    support::{Frame, Harness},
};
use crate::{
    analytical::test_support::{compiler_admission_investigation_fixture, query_compilation_evidence},
    capture::CollectionCancellation,
    diagnostics::analyze_pending,
    fleet::{journal::FocusedStage, scheduler::Outcome},
    outbox::DiagnosticInputState,
};
use culprit_core::telemetry::*;
use std::sync::atomic::Ordering;

mod failures;
pub(super) mod log_rates;
pub(super) mod query_history;
mod recovery;
pub(super) mod temporary;

pub(super) fn frame(focus: &SourceReadFocus, execution: &NativeSourceReadScope) -> Frame {
    let (_, records) = query_compilation_evidence();
    let source = culprit_core::source_id::QUERY_COMPILATION;
    Frame {
        source: CollectionDescription {
            target: focus.target.clone(),
            partition: SourcePartition::Target,
            source: "fixture-source-read-compilation".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::SourceRead {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::QueryCompilation,
                    records: records.len() as u32,
                    observed_from: "2020-01-01T12:00:02Z".into(),
                    observed_until: "2020-01-01T12:00:03Z".into(),
                    sources: vec![StateSourceCoverage::observed(source.into(), records.len() as u32, false)],
                },
                focus_key: focus.key().unwrap(),
                execution: Box::new(NativeReadContext {
                    target: focus.target.clone(),
                    partition: execution.partition.clone(),
                    database_writable: Some(execution.database_writable),
                    observed_from: "2020-01-01T12:00:02Z".into(),
                    observed_until: "2020-01-01T12:00:03Z".into(),
                }),
            },
        },
        records,
    }
}

async fn fixture() -> Harness {
    fixture_with_fence(None).await
}

async fn fixture_with_fence(fence: Option<&crate::delivery::recovery::Fence>) -> Harness {
    let compiler = compiler_admission_investigation_fixture();
    let mut source = compiler.baseline_source;
    let CollectionTarget::Database(database) = &mut source.target else { panic!() };
    // Explicit source-owned fixture fact, present before its first native capture and all receipt creation.
    database.incarnation = Some("controlled-native-logical-query-store-database".into());
    configured_fixture(Frame { source, records: vec![compiler.baseline_record.record] }, fence).await
}

async fn configured_fixture(activity: Frame, fence: Option<&crate::delivery::recovery::Fence>) -> Harness {
    let mut fixture = match fence {
        Some(fence) => Harness::with_archived_activity(activity, fence).await,
        None => Harness::with_activity(activity).await,
    };
    let scope = DiagnosticScope::from_source(&fixture.backend.activity.source).unwrap();
    fixture.backend.recipe.selector = RecipeSelector::FocusedCompilationAdmission;
    fixture.backend.recipe.id = "fixture.focused-compiler-prerequisite".into();
    let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.recipe.cost else { panic!() };
    receipt.selector = fixture.backend.recipe.selector;
    receipt.recipe_id = fixture.backend.recipe.id.clone();
    receipt.scope = scope.into();
    receipt.tested_budget.max_snapshot_rows = 64;
    receipt.tested_budget.page_rows = 64;
    let mut history = fixture.backend.recipe.clone();
    history.id = "fixture.automatic-compilation-history".into();
    history.selector = SourceReadQuestion::CompilationHistory.selector();
    let RecipeCostQualification::Measured(receipt) = &mut history.cost else { panic!() };
    receipt.recipe_id = history.id.clone();
    receipt.selector = history.selector;
    receipt.scope = RecipeCostScope::NativeRead(
        NativeSourceReadScope::from_context(
            &NativeReadContext::from_activity(&fixture.backend.activity.source).unwrap(),
        )
        .unwrap()
        .into(),
    );
    fixture.backend.additional_recipes.push(history);
    fixture
}

async fn gateway_answer(runtime: &mut Runtime) -> DiagnosticEpisode {
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = result.outcome else {
        panic!("a genuine bounded gateway answer")
    };
    runtime.finish(Outcome::FocusedComplete { result: capture }).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    runtime.reconcile().await;
    episode(runtime).await
}
async fn episode(runtime: &mut Runtime) -> DiagnosticEpisode {
    runtime
        .fixture
        .outbox
        .diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 8)
        .await
        .unwrap()
        .checkpoints
        .into_iter()
        .flat_map(|checkpoint| checkpoint.episodes)
        .find(|episode| matches!(episode.signal, DiagnosticSignal::CompilationAdmission { .. }))
        .unwrap()
}

#[tokio::test]
async fn finite_native_history_question_preserves_gateway_answer_and_retains_one_control_capture_not_rows() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    let original = gateway_answer(&mut runtime).await;
    assert!(original.source_read.as_ref().unwrap().needs_work());
    assert_eq!(original.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert!(runtime.enroll().await);
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    let intent =
        intents.iter().find(|intent| matches!(intent.focus, DiagnosticFocus::SourceRead { .. })).unwrap().clone();
    let stage = intent.episode_stage_key().unwrap();
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Complete);
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
    let control = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(control.parent, capture);
    assert!(control.rows.is_empty(), "history rows are not a diagnostic actor/result roster");
    assert_eq!(control.source.observation.snapshot().unwrap().records, 1);
    let context = NativeReadContext::from_snapshot(&control.source).unwrap();
    assert_eq!(context.partition, SourcePartition::Target);
    assert_eq!(context.database_writable, None);
    assert_eq!(episode(&mut runtime).await.source_read.unwrap().state, SourceReadStageState::Collecting);
    runtime.finish(Outcome::FocusedComplete { result: capture.clone() }).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let answered = episode(&mut runtime).await;
    assert_eq!(answered.report, original.report);
    assert_eq!(answered.analysis, original.analysis);
    assert_eq!(answered.first, original.first);
    assert_eq!(answered.source_read_capture(), Some(&capture));
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        0
    );
    runtime.reconcile().await;
    assert!(matches!(runtime.fixture.journal.focused_stage(&stage).await.unwrap(), Some(FocusedStage::Completed)));
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), [3; 5]);
}
