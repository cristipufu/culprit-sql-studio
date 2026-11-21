//! Automatic question planning/capture/CAS over controlled originals; no cost/native qualification.
use super::*;
use crate::analytical::test_support::{
    DiagnosticOriginalCase, QueryCompilationFollowupCase, diagnostic_original_evidence,
    query_compilation_followup_result,
};
use crate::fleet::investigations::tests::support::capture_frame;

pub(in crate::fleet::investigations) fn frame(
    original: &Frame,
    focus: &SourceReadFocus,
    execution: &SourceReadExecutionScope,
) -> Frame {
    let (source, records) = query_compilation_followup_result(
        &original.source,
        &original.records,
        focus,
        execution,
        QueryCompilationFollowupCase::Exact,
    );
    Frame { source, records }
}
async fn fixture() -> Harness {
    fixture_with_fence(None).await
}
async fn fixture_with_fence(fence: Option<&crate::delivery::recovery::Fence>) -> Harness {
    let (source, records) = diagnostic_original_evidence(DiagnosticOriginalCase::HistoryException);
    let frame = Frame { source, records };
    let mut fixture = match fence {
        Some(fence) => Harness::with_archived_query_history(frame, fence).await,
        None => Harness::with_query_history(frame).await,
    };
    fixture.backend.recipe.id = "fixture.automatic-query-compilation".into();
    fixture.backend.recipe.selector = SourceReadQuestion::CompilationHistory.selector();
    let execution = SourceReadExecutionScope::from_context(
        SourceReadQuestion::CompilationHistory,
        &SourceReadExecutionScope::context_from_source(&fixture.backend.activity.source).unwrap(),
    )
    .unwrap();
    let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.recipe.cost else { panic!() };
    receipt.recipe_id = fixture.backend.recipe.id.clone();
    receipt.selector = fixture.backend.recipe.selector;
    receipt.scope = RecipeCostScope::NativeRead(execution);
    fixture
}
async fn episode(runtime: &Runtime) -> DiagnosticEpisode {
    runtime
        .fixture
        .outbox
        .diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 8)
        .await
        .unwrap()
        .checkpoints
        .into_iter()
        .flat_map(|checkpoint| checkpoint.episodes)
        .find(|episode| {
            matches!(
                episode.signal,
                DiagnosticSignal::OriginalObservation {
                    condition: DiagnosticOriginalCondition::MssqlQueryHistory { .. },
                    ..
                }
            )
        })
        .unwrap()
}

#[tokio::test]
async fn failed_query_history_automatically_captures_one_existing_compilation_reader_and_persists_exact_answer() {
    let mut runtime = Runtime::from_captured_fixture(fixture().await).await;
    let original = episode(&runtime).await;
    assert!(matches!(original.source_read.as_ref().unwrap().state, SourceReadStageState::Deferred { .. }));
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = result.outcome else {
        panic!("one confirmed native-source-shaped answer")
    };
    runtime.finish(Outcome::FocusedComplete { result: capture.clone() }).await;
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(input.rows.len(), 1, "the exact canonical body is retained by the existing full verifier");
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let answered = episode(&runtime).await;
    let SourceReadStageState::Answered { answer } = &answered.source_read.as_ref().unwrap().state else { panic!() };
    let SourceReadAnswer::QueryCompilation(answer) = answer.as_ref() else { panic!() };
    assert!(matches!(answer.selection, QueryCompilationSelection::Observed { .. }));
    assert_eq!(answer.unresolved, QueryCompilationGap::ALL);
    assert_eq!(answered.first, original.first);
    assert_eq!(answered.report, original.report);
    assert_eq!(answered.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(answered.lifecycle, DiagnosticLifecycle::Uncertain);
    runtime.reconcile().await;
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), [1; 5]);
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
}

#[tokio::test]
async fn completed_question_survives_later_fragments_and_empty_history_without_reopening_or_resolving_the_group() {
    let mut runtime = Runtime::from_captured_fixture(fixture().await).await;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
    runtime.finish(Outcome::FocusedComplete { result: capture.clone() }).await;
    let policies = runtime.policies();
    analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
        .await
        .unwrap();
    runtime.reconcile().await;
    let before = episode(&runtime).await;
    for case in [DiagnosticOriginalCase::HistoryMultipleFragments, DiagnosticOriginalCase::HistoryZero] {
        let (mut source, records) = diagnostic_original_evidence(case);
        let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
        let owner = history.acquisition_owner.as_mut().unwrap();
        owner.observed_from = "2020-01-01T12:00:05.000000000Z".into();
        owner.observed_until = "2020-01-01T12:00:06.000000000Z".into();
        capture_frame(&mut runtime.fixture.outbox, Frame { source: source.clone(), records }).await;
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap();
        runtime.investigations.bindings.observe("target", &source, &runtime.fixture.backend.revision).unwrap();
        let after = episode(&runtime).await;
        assert_eq!(after.source_read, before.source_read);
        assert_eq!(after.first, before.first);
        assert_eq!(after.lifecycle, DiagnosticLifecycle::Uncertain);
        assert!(after.recovery.is_none());
        assert!(!runtime.enroll().await);
    }
    assert_eq!(runtime.native_calls(), [1; 5]);
}

#[tokio::test]
async fn unqualified_cancelled_or_changed_execution_routes_never_open_the_question_reader() {
    for reason in ["cost", "cancel", "process", "role"] {
        let mut fixture = fixture().await;
        match reason {
            "cost" => fixture.backend.recipe.cost = RecipeCostQualification::Unqualified,
            "process" | "role" => {
                let SourceObservation::QueryHistory(history) = &mut fixture.backend.activity.source.observation else {
                    panic!()
                };
                let owner = history.acquisition_owner.as_mut().unwrap();
                if reason == "role" {
                    owner.database_writable = Some(false)
                } else {
                    let SourcePartition::InstanceLocal { epoch, .. } = &mut owner.partition else { panic!() };
                    *epoch = Some("replacement-native-process".into());
                }
                fixture
                    .bindings
                    .observe("target", &fixture.backend.activity.source, &fixture.backend.revision)
                    .unwrap();
            }
            _ => (),
        }
        let cancel = CollectionCancellation::default();
        if reason == "cancel" {
            cancel.cancel()
        }
        assert!(fixture.prepare(&cancel).await.unwrap().is_empty(), "{reason}");
        assert_eq!(fixture.backend.calls.opened.load(Ordering::SeqCst), 0);
    }
}

async fn large_runtime(fence: Option<&crate::delivery::recovery::Fence>) -> Runtime {
    let mut fixture = fixture_with_fence(fence).await;
    let mut budget = fixture.backend.recipe.budget.clone();
    // A bounded controlled source response, within the existing ordinary caller envelope. Not native qualification.
    budget.page_rows = 64;
    budget.max_snapshot_rows = 64;
    fixture.backend.recipe.budget = budget.clone();
    let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.recipe.cost else { panic!() };
    receipt.tested_budget = budget;
    let mut runtime = Runtime::from_captured_fixture(fixture).await;
    assert!(runtime.enroll().await);
    let intent = runtime
        .fixture
        .journal
        .focused_intents()
        .await
        .unwrap()
        .into_iter()
        .find(|intent| matches!(intent.focus, DiagnosticFocus::SourceRead { .. }))
        .unwrap();
    let DiagnosticFocus::SourceRead { focus } = &intent.focus else { panic!() };
    let DiagnosticExecutionBinding::NativeRead { execution } = &intent.execution else { panic!() };
    let (source, records) = query_compilation_followup_result(
        &runtime.fixture.backend.activity.source,
        &runtime.fixture.backend.activity.records,
        focus,
        execution,
        QueryCompilationFollowupCase::LargeUnrelated,
    );
    runtime.fixture.backend.source_read_result = Some(Frame { source, records });
    runtime
}

#[tokio::test]
async fn large_unrelated_roster_keeps_one_original_position_through_local_restart_and_checks_bytes_after_selection() {
    let mut runtime = large_runtime(None).await;
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
    let root = runtime.fixture._directory.path().join("outbox");
    let manifest =
        crate::outbox::Outbox::inspect_manifest(&root, &capture.capture.observation_id).await.unwrap().unwrap();
    let file = manifest.files.iter().find(|file| file.stream == TelemetryStream::OperationalSamples).unwrap();
    assert!(file.bytes > 4 * 1024 * 1024, "the original entire file exceeds the unchanged restored-input allowance");
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(input.rows.len(), 1);
    assert_eq!(input.rows[0].reference.row_ordinal, 20);
    let expected = input.rows[0].reference.clone();
    runtime = super::recovery::restart(runtime, 0).await;
    let reopened = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(reopened.rows[0].reference, expected);
    // Tamper with unrelated SQL after the selected original. A selected prefix is never published before EOF.
    let path = root.join("captures").join(&capture.capture.observation_id).join(&file.file);
    let mut bytes = std::fs::read(&path).unwrap();
    let at = bytes.windows(b"column_".len()).rposition(|text| text == b"column_").unwrap() + b"column_".len();
    bytes[at] = b'X';
    std::fs::write(&path, bytes).unwrap();
    assert!(
        runtime
            .fixture
            .outbox
            .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .is_err()
    );
    assert_eq!(runtime.native_calls(), [1; 5]);
}

#[tokio::test]
async fn large_pending_original_cold_restore_keeps_selected_ordinal_and_cancelled_analysis_cannot_open_a_source() {
    use crate::delivery::{
        contract::Contract,
        diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_input},
        file_store::FileRecoveryStore,
        recovery::RecoveryStore,
    };
    let archive = tempfile::tempdir().unwrap();
    let contract = Contract::new(
        "pipeline".into(),
        "one".into(),
        "file:query-compilation".into(),
        digest(b"query-compilation-contract"),
    )
    .unwrap();
    let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut runtime = large_runtime(Some(owner.fence())).await;
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    let expected = input.rows[0].reference.clone();
    assert_eq!(expected.row_ordinal, 20);
    let archived = super::recovery::archive_ready(&mut runtime, &store, owner.fence(), &contract).await;
    let result_range = &archived[&capture.capture.observation_id].manifest().range_id;
    let head = store.diagnostic_heads(None, 10).await.unwrap().heads.remove(0);
    let cold_directory = tempfile::tempdir().unwrap();
    let mut cold = crate::outbox::Outbox::create_remote(
        &cold_directory.path().join("outbox"),
        OutboxLimits::default(),
        store.id(),
    )
    .unwrap();
    cold.recover().await.unwrap();
    cold.restore_diagnostics(load_diagnostic_checkpoint(&store, &head).await.unwrap()).await.unwrap();
    cold.restore_diagnostic_input("saved-source", load_diagnostic_input(&store, result_range).await.unwrap(), &store)
        .await
        .unwrap();
    let sequence = store.recover(None, 1).await.unwrap().high_sequence;
    cold.begin_diagnostic_epoch(owner.fence(), sequence).await.unwrap();
    let restored = cold.diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    assert_eq!(restored.rows.len(), 1);
    assert_eq!(restored.rows[0].reference, expected);
    let stopped = CollectionCancellation::default();
    stopped.cancel();
    let policies = runtime.policies();
    assert_eq!(analyze_pending(&mut cold, &policies, &stopped, 10, Some(&store)).await.unwrap(), 0);
    assert_eq!(
        cold.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Pending
    );
    assert_eq!(
        analyze_pending(&mut cold, &policies, &CollectionCancellation::default(), 10, Some(&store)).await.unwrap(),
        1
    );
    let answered = cold
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap()
        .checkpoint
        .unwrap()
        .episodes
        .remove(0);
    let SourceReadStageState::Answered { answer } = answered.source_read.unwrap().state else { panic!() };
    let SourceReadAnswer::QueryCompilation(answer) = *answer else { panic!() };
    let QueryCompilationSelection::Observed { observation, .. } = answer.selection else { panic!() };
    assert_eq!(observation, expected);
    assert_eq!(runtime.native_calls(), [1; 5]);
}
