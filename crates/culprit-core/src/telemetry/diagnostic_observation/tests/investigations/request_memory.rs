//! Synthetic structural receipts exercise binding only; no unit fixture qualifies native production work.
use super::*;

fn memory_intent() -> (DiagnosticInvestigationIntent, DiagnosticEpisode, RecipeDescriptor, ComponentVersion) {
    let mut scenario = scenario();
    let mut row = actor();
    let wait = row.wait.as_mut().unwrap();
    wait.class = Some(ActivityWaitClass::ExecutionGrant);
    wait.identified_resources.clear();
    let baseline = super::super::request_memory::memory_frame(1, vec![row], true);
    let observed = observe(&baseline, None);
    let episode = observed
        .checkpoint
        .episodes
        .iter()
        .find(|episode| matches!(episode.signal, DiagnosticSignal::RequestMemory { .. }))
        .unwrap()
        .clone();
    let DiagnosticFocusPlan::Ready(focus) =
        episode.plan_focus(&baseline.0, &baseline.2[0].1, &baseline.2[0].0).unwrap()
    else {
        panic!()
    };
    scenario.recipe.selector = RecipeSelector::FocusedRequestMemory;
    scenario.receipt().selector = RecipeSelector::FocusedRequestMemory;
    scenario.receipt().scope = episode.scope.clone().into();
    let intent = DiagnosticInvestigationIntent::new(
        &episode,
        &scenario.recipe,
        *focus,
        &scenario.component,
        &CollectionBudget::default(),
    )
    .unwrap();
    (intent, episode, scenario.recipe, scenario.component)
}

#[test]
fn the_measured_question_selector_cannot_be_reinterpreted_under_the_same_recipe_identity() {
    let (intent, episode, mut recipe, component) = memory_intent();
    assert!(intent.matches_admission(&episode, &recipe, &component, &CollectionBudget::default()));
    let original_key = intent.qualification.key().unwrap();
    recipe.selector = RecipeSelector::FocusedPageWait;
    assert!(!recipe.is_valid());
    assert!(!intent.matches_admission(&episode, &recipe, &component, &CollectionBudget::default()));
    let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
    receipt.selector = RecipeSelector::FocusedPageWait;
    assert_ne!(receipt.key().unwrap(), original_key, "the source's immutable measured approval includes its question");
    assert!(!intent.matches_admission(&episode, &recipe, &component, &CollectionBudget::default()));
}

#[test]
fn request_memory_lineage_reconstructs_only_its_exact_original_request_and_tested_budget() {
    let (intent, _, _, _) = memory_intent();
    let owner = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    let mut request =
        CollectionRequest { subject: intent.subject(), selection: intent.selection(), budget: intent.budget.clone() };
    let mut manifest = CollectionManifest {
        investigation: Some(owner.clone()),
        request: request.clone(),
        ..owner_manifest(&scenario())
    };
    // The manifest helper's other native fields do not establish this owner relation.
    assert_eq!(manifest.investigation_intent().unwrap().unwrap().key().unwrap(), intent.key().unwrap());
    request.budget.max_queries += 1;
    assert!(!owner.matches_request(&request));
    manifest.request = request;
    assert!(manifest.investigation_intent().is_err());
    let mut changed = intent.focus.clone();
    changed.participant_mut().original.key.request.as_mut().unwrap().generation = "replaced-request".into();
    assert!(owner.reconstruct(&changed).is_err());
}

#[test]
fn memory_stage_is_finite_and_recipe_or_receipt_versions_do_not_make_competing_questions() {
    let (intent, _, _, _) = memory_intent();
    let mut newer = intent.clone();
    newer.recipe_version += 1;
    newer.qualification.recipe_version = newer.recipe_version;
    assert!(newer.is_valid());
    assert_ne!(newer.key().unwrap(), intent.key().unwrap());
    assert_eq!(newer.episode_stage_key().unwrap(), intent.episode_stage_key().unwrap());
    assert_ne!(scenario().intent().episode_stage_key().unwrap(), intent.episode_stage_key().unwrap());
}
