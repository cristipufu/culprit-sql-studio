use super::*;
use crate::fleet::investigations::tests::page_writers;

async fn mapped_episode(harness: &Harness) -> DiagnosticEpisode {
    let checkpoints = harness.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    checkpoints
        .checkpoints
        .into_iter()
        .flat_map(|checkpoint| checkpoint.episodes)
        .find(|episode| matches!(episode.report, Some(DiagnosticReport::PageWait { .. })))
        .unwrap()
}

#[tokio::test]
async fn a_mapping_receipt_and_original_frame_are_authenticated_before_the_writer_cost_or_native_source() {
    let mut harness = page_writers::mapped_harness().await;
    let original = mapped_episode(&harness).await;
    let before = native_calls(&harness);
    let costs = harness.backend.verified.load(Ordering::SeqCst);
    let cancellation = CollectionCancellation::default();
    let mut evidence = EvidencePlanner {
        backend: &harness.backend,
        journal: &harness.journal,
        recovery: None,
        cancellation: &cancellation,
        population_frames: BTreeMap::new(),
        execution_owners: BTreeMap::new(),
    };
    let mut forged = original.clone();
    let Some(DiagnosticReport::PageWait { observation, .. }) = &mut forged.report else { panic!() };
    observation.payload_sha256 = digest(b"different mapped page receipt");
    let result = evidence
        .focus(
            &mut harness.outbox,
            &harness.inventory.targets[0],
            &forged,
            harness.bindings.get("target").unwrap(),
            RecipeSelector::FocusedPageWaitWriters,
        )
        .await;
    assert!(result.is_err());
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), costs);
    assert_eq!(native_calls(&harness), before);
    assert!(harness.journal.focused_intents().await.unwrap().is_empty());
}

#[tokio::test]
async fn unavailable_archived_page_evidence_withholds_the_optional_stage_without_native_work() {
    let mut harness = page_writers::mapped_harness().await;
    let mut episode = mapped_episode(&harness).await;
    let Some(DiagnosticReport::PageWait { observation, .. }) = &mut episode.report else { panic!() };
    observation.capture.observation_id = "unavailable-original-page-capture".into();
    observation.batch_id = batch_id(&observation.capture.observation_id, observation.stream).unwrap();
    assert!(episode.is_valid());
    let cancellation = CollectionCancellation::default();
    let mut evidence = EvidencePlanner {
        backend: &harness.backend,
        journal: &harness.journal,
        recovery: None,
        cancellation: &cancellation,
        population_frames: BTreeMap::new(),
        execution_owners: BTreeMap::new(),
    };
    let before = native_calls(&harness);
    assert!(
        evidence
            .focus(
                &mut harness.outbox,
                &harness.inventory.targets[0],
                &episode,
                harness.bindings.get("target").unwrap(),
                RecipeSelector::FocusedPageWaitWriters,
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(native_calls(&harness), before);
    assert!(harness.journal.focused_intents().await.unwrap().is_empty());
}

#[tokio::test]
async fn no_writer_recipe_or_wrong_cost_selector_can_borrow_the_mapping_approval() {
    for policy in ["missing", "unqualified", "selector", "cost", "generation", "budget"] {
        let mut harness = page_writers::mapped_harness().await;
        let backend = &mut harness.backend;
        let writer = &mut backend.additional_recipes[0];
        match policy {
            "missing" => backend.additional_recipes.clear(),
            "unqualified" => writer.cost = RecipeCostQualification::Unqualified,
            "selector" => {
                writer.selector = RecipeSelector::FocusedCompilationAdmission;
                let RecipeCostQualification::Measured(receipt) = &mut writer.cost else { panic!() };
                receipt.selector = writer.selector;
            }
            "cost" => {
                let RecipeCostQualification::Measured(receipt) = &mut writer.cost else { panic!() };
                receipt.evidence.payload_sha256 = digest(b"wrong independent writer experiment");
            }
            "generation" => {
                let RecipeCostQualification::Measured(receipt) = &mut writer.cost else { panic!() };
                let RecipeCostScope::Activity(scope) = &mut receipt.scope else { panic!() };
                scope.native_epoch = Some("another-native-generation".into());
                let SourcePartition::InstanceLocal { epoch, .. } = &mut scope.partition else { panic!() };
                *epoch = scope.native_epoch.clone();
            }
            _ => harness.inventory.targets[0].schedules[0].budget.max_queries = 1,
        }
        let before = native_calls(&harness);
        let costs = harness.backend.verified.load(Ordering::SeqCst);
        let result = harness.prepare(&CollectionCancellation::default()).await;
        if policy == "cost" {
            assert!(result.is_err(), "corrupt evidence is never treated as ordinary unavailability");
        } else {
            assert!(result.unwrap().is_empty(), "{policy}");
        }
        assert_eq!(native_calls(&harness), before, "{policy}");
        assert_eq!(harness.backend.verified.load(Ordering::SeqCst), costs, "{policy}");
        assert!(harness.journal.focused_intents().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn cancellation_before_or_during_writer_cost_verification_never_enrolls_or_dispatches_a_source() {
    for during_verification in [false, true] {
        let mut harness = page_writers::mapped_harness().await;
        let cancellation = CollectionCancellation::default();
        if during_verification {
            harness.backend.cancel = Some(cancellation.clone());
        } else {
            cancellation.cancel();
        }
        let before = native_calls(&harness);
        assert!(harness.prepare(&cancellation).await.unwrap().is_empty());
        assert_eq!(native_calls(&harness), before);
        assert!(harness.journal.focused_intents().await.unwrap().is_empty());
    }
}

fn native_calls(harness: &Harness) -> [usize; 5] {
    let calls = &harness.backend.calls;
    [&calls.captured, &calls.opened, &calls.observed, &calls.verified, &calls.closed]
        .map(|count| count.load(Ordering::SeqCst))
}
