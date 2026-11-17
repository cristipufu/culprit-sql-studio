use super::*;
use crate::fleet::investigations::tests::page_task_holders::{paired_harness, task_episode};

#[tokio::test]
async fn a_task_point_prepares_only_its_holder_question_from_the_two_exact_original_receipts() {
    let mut harness = paired_harness().await;
    let episode = task_episode(&harness).await;
    assert_eq!(possible_questions(&episode), [RecipeSelector::FocusedPageTaskHolders]);
    let before = harness.backend.calls.opened.load(Ordering::SeqCst);
    let prepared = harness.prepare(&CollectionCancellation::default()).await.unwrap();
    let [candidate] = prepared.as_slice() else { panic!("one native task question") };
    let DiagnosticFocus::PageTaskHolders { focus } = &candidate.intent.focus else { panic!() };
    assert!(episode.matches_page_task_holder_focus(focus.as_ref()));
    assert_eq!(candidate.job.cooldown_seconds, 60);
    assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), before);
    assert!(harness.journal.focused_intents().await.unwrap().is_empty(), "preparation is not enrollment");
}

#[tokio::test]
async fn task_cost_unknown_route_and_stricter_budget_are_withheld_before_native_dispatch() {
    for reason in ["cost", "route", "budget"] {
        let mut harness = paired_harness().await;
        match reason {
            "cost" => harness.backend.recipe.cost = RecipeCostQualification::Unqualified,
            "route" => harness.backend.changed.store(true, Ordering::SeqCst),
            _ => harness.inventory.targets[0].schedules[0].budget.max_queries = 1,
        }
        let before = harness.backend.calls.opened.load(Ordering::SeqCst);
        assert!(harness.prepare(&CollectionCancellation::default()).await.unwrap().is_empty(), "{reason}");
        assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), before, "{reason}");
        assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 0, "{reason}");
        assert!(harness.journal.focused_intents().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn missing_original_task_page_never_chooses_a_current_actor_or_opens_a_source() {
    let mut harness = paired_harness().await;
    let mut episode = task_episode(&harness).await;
    let DiagnosticSignal::PageTaskWait { page, .. } = &mut episode.signal else { panic!() };
    page.capture.observation_id = "unavailable-original-task-page".into();
    page.batch_id = batch_id(&page.capture.observation_id, page.stream).unwrap();
    episode.first = page.as_ref().clone();
    episode.latest = page.as_ref().clone();
    episode.id = episode.expected_id().unwrap();
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
    assert!(
        evidence
            .focus(
                &mut harness.outbox,
                &harness.inventory.targets[0],
                &episode,
                RecipeSelector::FocusedPageTaskHolders,
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 0);
}
