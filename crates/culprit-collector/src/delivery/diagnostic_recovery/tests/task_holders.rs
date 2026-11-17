//! Synthetic native inputs use the existing manifest writer and actual recovery commit/EOF verification.
use super::*;
use crate::analytical::test_support::{PageTaskHolderCase, page_task_holder_fixture};

struct ManualArchive {
    root: tempfile::TempDir,
    store: FileRecoveryStore,
    owner: FileOwnership,
    contract: Contract,
    original: RangeManifest,
    page: RangeManifest,
    source: CollectionDescription,
    record: TelemetryRecord,
    focus: PageTaskHolderFocus,
}

impl ManualArchive {
    async fn new() -> Self {
        Self::with_long_running(false).await
    }

    async fn with_long_running(long_running: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let contract = Contract::new(
            "pipeline".into(),
            "shard".into(),
            "file:manual-task-proof".into(),
            digest(b"manual-task-schema"),
        )
        .unwrap();
        let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let mut fixture = page_task_holder_fixture(PageTaskHolderCase::QualifiedBoth);
        if long_running {
            let TelemetryRecord::Activity(actor) = &mut fixture.activity_rows[0] else { panic!() };
            actor.elapsed_ms = Some("120000".into());
        }
        let SourceObservation::PageTaskHolders { focus: template, .. } = &fixture.source.observation else { panic!() };
        let original = prepared(
            root.path(),
            &contract,
            template.participant.parent.capture.clone(),
            request(CollectionSelection::Activity),
            None,
            fixture.activity_source.clone(),
            fixture.activity_rows.clone(),
        );
        let original = stage_and_commit(&store, owner.fence(), None, &original).await.unwrap();
        let original_rows = fixture
            .activity_rows
            .iter()
            .enumerate()
            .map(|(ordinal, record)| {
                let exported =
                    ExportedRecord::new(&fixture.activity_source, &original.manifest().capture, record.clone())
                        .unwrap();
                let reference = DiagnosticEvidenceRef::from_export(
                    &fixture.activity_source,
                    &original.manifest().capture_manifest_sha256,
                    &exported,
                    ordinal as u32,
                )
                .unwrap();
                (reference, exported)
            })
            .collect::<Vec<_>>();
        let page = prepared(
            root.path(),
            &contract,
            template.page.capture.clone(),
            request(CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets }),
            None,
            fixture.page_source.clone(),
            fixture.page_rows.clone(),
        );
        let page = stage_and_commit(&store, owner.fence(), Some(&original.manifest().range_id), &page).await.unwrap();
        let exported =
            ExportedRecord::new(&fixture.page_source, &page.manifest().capture, fixture.page_rows[0].clone()).unwrap();
        let reference = DiagnosticEvidenceRef::from_export(
            &fixture.page_source,
            &page.manifest().capture_manifest_sha256,
            &exported,
            0,
        )
        .unwrap();
        let activity = original_rows
            .iter()
            .map(|(reference, row)| {
                let TelemetryRecord::Activity(observation) = &row.record else { panic!() };
                ActivityFrameRow { observation, reference: Some(reference) }
            })
            .collect::<Vec<_>>();
        let frame = ActivityFrameEvidence::new(&fixture.activity_source, &activity, true).unwrap();
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(point), .. }) =
            &exported.record
        else {
            panic!()
        };
        let DiagnosticFocusPlan::Ready(question) =
            plan_page_task_holder_focus(&fixture.page_source, point, &reference, &frame).unwrap()
        else {
            panic!()
        };
        let DiagnosticFocus::PageTaskHolders { focus } = *question else { panic!() };
        Self {
            root,
            store,
            owner,
            contract,
            original: original.manifest().clone(),
            page: page.manifest().clone(),
            source: fixture.page_source,
            record: fixture.page_rows[0].clone(),
            focus: *focus,
        }
    }
}

fn request(selection: CollectionSelection) -> CollectionRequest {
    CollectionRequest { subject: CollectionSubject::Database, selection, budget: Default::default() }
}

#[tokio::test]
async fn explicit_manual_unpaired_focus_authenticates_after_cold_reopen_but_cannot_be_an_automatic_task_episode() {
    let fixture = ManualArchive::new().await;
    // All reads below go through the recovery store; no local original outbox or native database is available.
    let cold = FileRecoveryStore::open(&fixture.root.path().join("recovery"), fixture.contract.clone()).unwrap();
    super::super::task_holders::authenticate_focus(&cold, &fixture.contract, &fixture.focus).await.unwrap();
    let episode = DiagnosticEpisode::start(
        DiagnosticScope::from_source(&fixture.original.source).unwrap(),
        DiagnosticSignal::PageTaskWait {
            key: fixture.focus.participant.original.key.clone(),
            statement_identity: fixture.focus.participant.statement_identity.clone(),
            parent: Box::new(fixture.focus.participant.parent.clone()),
            page: Box::new(fixture.focus.page.clone()),
            task: Box::new(fixture.focus.task.clone()),
            database_writable: true,
        },
        fixture.focus.task.observed_at.clone(),
        fixture.focus.page.clone(),
    )
    .unwrap();
    assert!(episode.is_valid());
    assert!(
        super::super::task_holders::authenticate_episode(&fixture.store, &fixture.contract, &episode).await.is_err(),
        "None is useful manual evidence, never a pre-native paired incident declaration"
    );
}

#[tokio::test]
async fn a_paired_header_with_another_original_manifest_cannot_authenticate_the_same_native_actor() {
    let fixture = ManualArchive::new().await;
    let mut source = fixture.source.clone();
    let mut other = capture_reference(&fixture.original).unwrap();
    other.manifest_sha256 = digest(b"not-the-original-manifest");
    let SourceObservation::PageWaitTargets { original_activity, .. } = &mut source.observation else { panic!() };
    *original_activity = Some(other.clone());
    let mut capture = fixture.page.capture.clone();
    capture.observation_id = "task-page-with-substituted-parent".into();
    capture.revision.as_mut().unwrap().sequence = "830004".into();
    let native = prepared(
        fixture.root.path(),
        &fixture.contract,
        capture,
        request(CollectionSelection::PageWaitTasks {
            activity: other,
            original_context: Box::new(NativeReadContext::from_activity(&fixture.original.source).unwrap()),
            original_requests: {
                let original = load_diagnostic_input(&fixture.store, &fixture.original.range_id).await.unwrap();
                let rows = super::super::input_rows(&original).unwrap();
                let activity = super::super::activity_rows(&rows).unwrap();
                ActivityFrameEvidence::new(&fixture.original.source, &activity, true)
                    .unwrap()
                    .page_wait_task_requests()
                    .unwrap()
            },
        }),
        None,
        source.clone(),
        vec![fixture.record.clone()],
    );
    let native =
        stage_and_commit(&fixture.store, fixture.owner.fence(), Some(&fixture.page.range_id), &native).await.unwrap();
    let row = ExportedRecord::new(&source, &native.manifest().capture, fixture.record.clone()).unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&source, &native.manifest().capture_manifest_sha256, &row, 0).unwrap();
    let mut focus = fixture.focus.clone();
    focus.page = reference;
    assert!(focus.is_valid());
    assert!(super::super::task_holders::authenticate_focus(&fixture.store, &fixture.contract, &focus).await.is_err());
}

#[tokio::test]
async fn cold_paired_question_authenticates_the_full_original_frame_and_rejects_a_changed_admitted_context() {
    let fixture = ManualArchive::with_long_running(true).await;
    let original = load_diagnostic_input(&fixture.store, &fixture.original.range_id).await.unwrap();
    let native_rows = super::super::input_rows(&original).unwrap();
    let activity = super::super::activity_rows(&native_rows).unwrap();
    let source = &original.range().manifest().source;
    let parent = capture_reference(original.range().manifest()).unwrap();
    let rows = activity
        .iter()
        .map(|row| DiagnosticActivityRow { reference: row.reference.unwrap(), observation: row.observation })
        .collect::<Vec<_>>();
    let observed =
        observe_diagnostic_activity(source, &parent, &rows, None, &ActivityPolicy::default(), "2020-01-01T12:00:10Z")
            .unwrap();
    let episode = observed
        .checkpoint
        .episodes
        .iter()
        .find(|episode| {
            matches!(episode.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
        })
        .unwrap();
    let frame = ActivityFrameEvidence::new(source, &activity, true).unwrap();
    let DiagnosticFocusPlan::Ready(question) = episode.plan_page_wait_tasks_focus(&frame).unwrap() else {
        panic!("actual sealed long-running participant")
    };
    // The independent cost fields are structural fixture data only. This read-only test authenticates original
    // prerequisites; it neither invokes a native cost verifier nor admits any source work.
    let mut recipe = crate::analytical::test_support::investigation_fixture().recipe;
    recipe.id = "fixture.archived-page-task-population".into();
    recipe.selector = RecipeSelector::PageWaitTasks;
    let RecipeCostQualification::Measured(cost) = &mut recipe.cost else { panic!() };
    cost.recipe_id = recipe.id.clone();
    cost.selector = recipe.selector;
    cost.scope = episode.scope.clone().into();
    let component = cost.component.clone();
    let intent =
        DiagnosticInvestigationIntent::new(episode, &recipe, *question, &component, &CollectionBudget::default())
            .unwrap();
    let mut authenticated = std::collections::BTreeSet::new();
    super::super::authenticate_investigation_intent(&fixture.store, &fixture.contract, &intent, &mut authenticated)
        .await
        .unwrap();
    assert!(authenticated.contains(&intent.key().unwrap()));
    let expected_requests = frame.page_wait_task_requests().unwrap();
    let selection_bytes = serde_json::to_vec(&intent.selection()).unwrap();
    let lineage: DiagnosticInvestigationLineage = serde_json::from_slice(
        &serde_json::to_vec(&DiagnosticInvestigationLineage::from_intent(&intent).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::to_vec(&lineage.reconstruct(&lineage.focus).unwrap().selection()).unwrap(), selection_bytes);
    for change in ["omitted", "injected", "token"] {
        let mut changed = intent.clone();
        let DiagnosticFocus::PageWaitTasks { original_requests, .. } = &mut changed.focus else { panic!() };
        match change {
            "omitted" => {
                original_requests.pop();
            }
            "injected" => {
                let mut extra = original_requests[0].clone();
                extra.key.session.id = "999".into();
                original_requests.push(extra);
                original_requests.sort_by(|left, right| left.key.cmp(&right.key));
            }
            _ => original_requests.last_mut().unwrap().statement_identity.id = "another-native-token".into(),
        }
        assert_ne!(*original_requests, expected_requests);
        assert!(changed.is_valid(), "{change}: structured but not the authenticated full roster");
        assert!(
            super::super::authenticate_investigation_intent(
                &fixture.store,
                &fixture.contract,
                &changed,
                &mut authenticated
            )
            .await
            .is_err(),
            "{change}"
        );
    }
    let mut changed = intent;
    let DiagnosticFocus::PageWaitTasks { original_context, .. } = &mut changed.focus else { panic!() };
    original_context.observed_from = "2019-01-01T00:00:00Z".into();
    assert!(changed.is_valid(), "structural consistency cannot authenticate the original native clock");
    assert!(
        super::super::authenticate_investigation_intent(
            &fixture.store,
            &fixture.contract,
            &changed,
            &mut authenticated
        )
        .await
        .is_err()
    );
}
