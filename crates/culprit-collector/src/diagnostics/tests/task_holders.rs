use super::*;
use crate::analytical::test_support::{PageTaskHolderCase, PageTaskHolderFixture, page_task_holder_fixture};
use crate::delivery::{
    contract::Contract,
    diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_input},
    file_store::FileRecoveryStore,
    recovery::RecoveryStore,
};

struct Scenario {
    harness: Harness,
    original: DiagnosticInput,
    page: DiagnosticInput,
    result: Frame,
    focus: PageTaskHolderFocus,
}

impl Scenario {
    async fn new(case: PageTaskHolderCase) -> Self {
        Self::with_harness(Harness::new().await, case).await
    }
    async fn with_harness(mut harness: Harness, case: PageTaskHolderCase) -> Self {
        let PageTaskHolderFixture { activity_source, activity_rows, mut page_source, page_rows, source, records } =
            page_task_holder_fixture(case);
        let original_capture = harness.capture(Frame { source: activity_source, records: activity_rows }).await;
        assert_eq!(harness.work(1).await, 1);
        let original = harness
            .outbox
            .diagnostic_input(&original_capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
            .await
            .unwrap();
        let SourceObservation::PageWaitTargets { original_activity, .. } = &mut page_source.observation else {
            panic!()
        };
        *original_activity = Some(original.parent.clone());
        let page_capture = harness.capture(Frame { source: page_source, records: page_rows }).await;
        let page =
            harness.outbox.diagnostic_input(&page_capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(point), .. }) =
            &page.rows[0].row.record
        else {
            panic!()
        };
        let DiagnosticFocusPlan::Ready(question) = plan_page_task_holder_focus(
            &page.source,
            point,
            &page.rows[0].reference,
            &original.activity_frame().unwrap(),
        )
        .unwrap() else {
            panic!("actual paired original receipts")
        };
        let DiagnosticFocus::PageTaskHolders { focus } = *question else { panic!() };
        let mut result = Frame { source, records };
        let SourceObservation::PageTaskHolders { focus: declared, .. } = &mut result.source.observation else {
            panic!()
        };
        *declared = focus.clone();
        let TelemetryRecord::Operational(row) = &mut result.records[0] else { panic!() };
        let OperationalFact::PageTaskHolders(point) = &mut row.fact else { panic!() };
        point.focus_key = focus.key().unwrap();
        row.key = point.key().unwrap();
        assert!(focus.accepts_observation(&NativeReadContext::from_snapshot(&result.source).unwrap(), point));
        Self { harness, original, page, result, focus: *focus }
    }
    async fn admit(&mut self) -> DiagnosticEpisode {
        assert_eq!(self.harness.work(1).await, 1);
        self.harness
            .checkpoint()
            .await
            .episodes
            .into_iter()
            .find(|episode| matches!(episode.signal, DiagnosticSignal::PageTaskWait { .. }))
            .unwrap()
    }
}

#[tokio::test]
async fn paired_intake_uses_the_actual_task_and_both_durable_originals_without_a_coordinator_wait() {
    let mut scenario = Scenario::new(PageTaskHolderCase::QualifiedBoth).await;
    let native_before = scenario.harness.calls.counts();
    let episode = scenario.admit().await;
    assert_eq!(scenario.harness.calls.counts(), native_before);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    assert!(episode.matches_page_task_holder_focus(&scenario.focus));
    assert_eq!(episode.first, scenario.page.rows[0].reference);
    let DiagnosticSignal::PageTaskWait { parent, page, task, .. } = &episode.signal else { panic!() };
    assert_eq!(parent.as_ref(), &scenario.focus.participant.parent);
    assert_eq!(page.as_ref(), &scenario.page.rows[0].reference);
    assert_eq!(task.as_ref(), &scenario.focus.task);
    assert_eq!(scenario.focus.participant.original.wait.as_ref().unwrap().native_type.as_deref(), Some("CXCONSUMER"));
    let revisions = scenario.harness.revisions(&scenario.page.parent.capture).await;
    let revision = revisions.iter().find(|revision| revision.episode.id == episode.id).unwrap();
    assert!(revision.inputs.contains(&scenario.focus.participant.parent));
    assert!(revision.inputs.contains(&scenario.page.rows[0].reference));
    assert_eq!(scenario.harness.work(1).await, 0, "a consumed paired population never re-admits incidents");
}

#[tokio::test]
async fn unpaired_task_population_stays_raw_evidence_and_is_not_a_pending_native_or_analysis_question() {
    let fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
    let mut harness = Harness::new().await;
    let original = harness.capture(Frame { source: fixture.activity_source, records: fixture.activity_rows }).await;
    assert_eq!(harness.work(1).await, 1);
    let source = fixture.page_source.clone();
    let capture = harness.capture(Frame { source: fixture.page_source, records: fixture.page_rows }).await;
    assert!(harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    assert_eq!(harness.work(1).await, 0);
    let directory = harness.root.path().join(format!("inspect-unpaired-{}", capture.observation_id));
    let rows = replay(&harness.path, &capture, &directory).await;
    let bytes =
        std::fs::read(harness.path.join("captures").join(&capture.observation_id).join("manifest.json")).unwrap();
    let reference = DiagnosticEvidenceRef::from_export(&source, &digest(&bytes), &rows[0], 0).unwrap();
    assert_eq!(harness.outbox.diagnostic_evidence(&reference).await.unwrap().1, rows[0]);
    assert!(
        !harness
            .checkpoint()
            .await
            .episodes
            .iter()
            .any(|episode| matches!(episode.signal, DiagnosticSignal::PageTaskWait { .. }))
    );
    assert_ne!(capture, original);
}

#[tokio::test]
async fn declaration_only_holder_recipe_defers_the_exact_question_without_native_work() {
    let mut scenario = Scenario::new(PageTaskHolderCase::QualifiedBoth).await;
    let recipe = RecipeDescriptor {
        id: "fixture.task-holders".into(),
        version: 1,
        purpose: RecipePurpose::Investigation,
        selector: RecipeSelector::FocusedPageTaskHolders,
        subjects: vec![RecipeSubject::Database],
        budget: CollectionBudget::default(),
        cost: RecipeCostQualification::Unqualified,
    };
    scenario.harness.policies =
        DiagnosticPolicies::for_activity("fixture", CollectionSubject::Database, ActivityPolicy::default())
            .unwrap()
            .with_recipes("fixture", vec![recipe])
            .unwrap();
    let before = scenario.harness.calls.counts();
    let episode = scenario.admit().await;
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::CostUnqualified));
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(scenario.harness.calls.counts(), before);
    assert!(episode.matches_page_task_holder_focus(&scenario.focus));
}

#[tokio::test]
async fn settled_holder_results_retire_only_their_point_question_and_leave_immutable_original_history() {
    for case in [
        PageTaskHolderCase::QualifiedBoth,
        PageTaskHolderCase::ChangedRequest,
        PageTaskHolderCase::PermissionUnavailable,
    ] {
        let mut scenario = Scenario::new(case).await;
        let original_episode = scenario.admit().await;
        let input = scenario.harness.capture(scenario.result.clone()).await;
        assert_eq!(scenario.harness.work(1).await, 1);
        assert!(!scenario.harness.checkpoint().await.episodes.iter().any(|episode| episode.id == original_episode.id));
        let history = scenario.harness.revisions(&input).await;
        let episode = &history.iter().find(|revision| revision.episode.id == original_episode.id).unwrap().episode;
        assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
        assert_eq!(episode.first, original_episode.first);
        assert_eq!(episode.first_seen_at, original_episode.first_seen_at);
        assert_eq!(episode.last_seen_at, original_episode.last_seen_at);
        assert!(episode.recovery.is_none());
        assert_eq!(
            episode.analysis,
            if case == PageTaskHolderCase::QualifiedBoth {
                DiagnosticAnalysisStatus::Analyzed
            } else {
                DiagnosticAnalysisStatus::Unresolved
            }
        );
        assert!(matches!(&episode.report,Some(DiagnosticReport::PageTaskHolders{page,parent,..})
            if page.as_ref()==&scenario.focus.page && parent.as_ref()==&scenario.focus.participant.parent));
        for prerequisite in [&scenario.focus.page, &scenario.focus.participant.parent] {
            assert!(
                history
                    .iter()
                    .find(|revision| revision.episode.id == original_episode.id)
                    .unwrap()
                    .inputs
                    .contains(prerequisite)
            );
        }
    }
}

#[tokio::test]
async fn cancellation_keeps_the_paired_input_pending_and_does_not_open_or_acknowledge_any_source() {
    let mut scenario = Scenario::new(PageTaskHolderCase::QualifiedBoth).await;
    let stop = CollectionCancellation::default();
    stop.cancel();
    let before = scenario.harness.calls.counts();
    assert_eq!(
        analyze_pending(&mut scenario.harness.outbox, &scenario.harness.policies, &stop, 1, None).await.unwrap(),
        0
    );
    assert_eq!(
        scenario.harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap().parent,
        scenario.page.parent
    );
    assert_eq!(scenario.harness.calls.counts(), before);
    assert!(scenario.admit().await.matches_page_task_holder_focus(&scenario.focus));
}

#[tokio::test]
async fn cold_pending_population_recovers_its_manifest_named_original_without_a_native_baseline() {
    let archive = tempfile::tempdir().unwrap();
    let contract = Contract::new(
        "pipeline".into(),
        "shard".into(),
        "file:warehouse".into(),
        digest(b"paired-task-recovery-schema"),
    )
    .unwrap();
    let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut hot = Harness::with_recovery_store(Some(store.id())).await;
    hot.outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    let scenario = Scenario::with_harness(hot, PageTaskHolderCase::QualifiedBoth).await;
    let parent = scenario.original.parent.clone();
    let page = scenario.page.parent.clone();
    super::recovery::archive_capture(&scenario.harness.outbox, &store, owner.fence(), &contract, &parent.capture).await;
    let derived = scenario.harness.outbox.status(50,None).await.unwrap().entries.into_iter()
        .find(|entry|matches!(&entry.request.selection,CollectionSelection::Diagnostics{parent:input,..} if input==&parent)).unwrap().capture;
    super::recovery::archive_capture(&scenario.harness.outbox, &store, owner.fence(), &contract, &derived).await;
    let native =
        super::recovery::archive_capture(&scenario.harness.outbox, &store, owner.fence(), &contract, &page.capture)
            .await;
    let pending = store.pending_diagnostic_inputs(None, 10).await.unwrap();
    assert!(pending.ranges.iter().any(|range| range.capture == page.capture));
    let heads = store.diagnostic_heads(None, 10).await.unwrap();
    let recovered = load_diagnostic_checkpoint(&store, &heads.heads[0]).await.unwrap();
    let native_before = scenario.harness.calls.counts();
    let calls = scenario.harness.calls.clone();
    drop(scenario);
    let mut cold = Harness::with_recovery_store(Some(store.id())).await;
    cold.calls = calls;
    cold.outbox.restore_diagnostics(recovered).await.unwrap();
    cold.outbox
        .restore_diagnostic_input(
            "fixture",
            load_diagnostic_input(&store, &native.manifest().range_id).await.unwrap(),
            &store,
        )
        .await
        .unwrap();
    cold.outbox.begin_diagnostic_epoch(owner.fence(), pending.high_sequence).await.unwrap();
    assert_eq!(
        analyze_pending(&mut cold.outbox, &cold.policies, &CollectionCancellation::default(), 1, Some(&store))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        cold.calls.counts(),
        native_before,
        "cold intake reads authenticated artifacts, never a new native baseline"
    );
    let checkpoint = cold.checkpoint().await;
    let episode = checkpoint
        .episodes
        .iter()
        .find(|episode| matches!(episode.signal, DiagnosticSignal::PageTaskWait { .. }))
        .unwrap();
    let DiagnosticSignal::PageTaskWait { parent: original, page: observed, .. } = &episode.signal else { panic!() };
    assert_eq!(original.capture_ref(), parent);
    assert_eq!(observed.capture_ref(), page);
    let restored =
        cold.outbox.diagnostic_input(&parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    assert_eq!(restored.parent, parent);
    assert_eq!(
        cold.outbox.diagnostic_input_state(&parent, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        crate::outbox::DiagnosticInputState::EvidenceOnly
    );
}
