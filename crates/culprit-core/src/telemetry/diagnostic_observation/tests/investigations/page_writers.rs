//! These structural cost fixtures do not qualify real native work or replace a registration's cost verifier.
use super::*;

fn writer_intent() -> (DiagnosticInvestigationIntent, DiagnosticEpisode, RecipeDescriptor, ComponentVersion) {
    let mut original = scenario();
    let (episode, focus) = super::super::page_writers::investigation_case();
    original.recipe.selector = RecipeSelector::FocusedPageWaitWriters;
    original.receipt().selector = RecipeSelector::FocusedPageWaitWriters;
    original.receipt().scope = episode.scope.clone().into();
    let intent = DiagnosticInvestigationIntent::new(
        &episode,
        &original.recipe,
        DiagnosticFocus::PageWaitWriters { focus: Box::new(focus) },
        &original.component,
        &CollectionBudget::default(),
    )
    .unwrap();
    (intent, episode, original.recipe, original.component)
}

#[test]
fn mapping_and_writer_questions_have_distinct_finite_stages_on_the_same_incident() {
    let (writer, episode, mut recipe, component) = writer_intent();
    let DiagnosticFocus::PageWaitWriters { focus } = &writer.focus else { panic!() };
    recipe.selector = RecipeSelector::FocusedPageWait;
    let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
    receipt.selector = RecipeSelector::FocusedPageWait;
    let mapping = DiagnosticInvestigationIntent::new(
        &episode,
        &recipe,
        focus.waiter.clone().into(),
        &component,
        &CollectionBudget::default(),
    )
    .unwrap();
    assert_eq!(writer.episode_id, mapping.episode_id);
    assert_ne!(writer.episode_stage_key().unwrap(), mapping.episode_stage_key().unwrap());
    let mut newer = writer.clone();
    newer.recipe_version += 1;
    newer.qualification.recipe_version = newer.recipe_version;
    assert!(newer.is_valid());
    assert_eq!(newer.episode_stage_key().unwrap(), writer.episode_stage_key().unwrap());
    assert_ne!(newer.key().unwrap(), writer.key().unwrap());
}

#[test]
fn a_mapping_cost_receipt_cannot_be_reinterpreted_as_writer_work_or_shared_ownership() {
    let (intent, episode, mut recipe, component) = writer_intent();
    let measured_key = intent.qualification.key().unwrap();
    let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
    receipt.selector = RecipeSelector::FocusedPageWait;
    assert_ne!(receipt.key().unwrap(), measured_key);
    assert!(!recipe.is_valid());
    assert!(!intent.matches_admission(&episode, &recipe, &component, &CollectionBudget::default()));
    let mut shared = intent;
    shared.execution = DiagnosticExecutionBinding::Shared { owner: Box::new(shared.qualification.evidence.clone()) };
    assert!(!shared.is_valid(), "a database writer question never borrows shared execution authority");
}

#[test]
fn a_stopped_writer_attempt_retains_page_and_all_original_candidates_before_any_native_result() {
    let (intent, before, _, _) = writer_intent();
    let mut next = before.clone();
    next.analysis = DiagnosticAnalysisStatus::Unresolved;
    next.issue = Some(DiagnosticAnalysisIssue::Interrupted);
    let event = DiagnosticInvestigationEvent {
        intent: Box::new(intent.clone()),
        attempt: None,
        source_work: SourceWorkState::NotStarted,
        outcome: DiagnosticInvestigationOutcome::Stopped { issue: DiagnosticAnalysisIssue::Interrupted },
    };
    let revision =
        DiagnosticRevision::with_investigation(&before, next, event, "2020-01-01T00:04:00Z").unwrap().unwrap();
    assert!(revision.is_valid());
    assert!(intent.focus.references().all(|reference| revision.inputs.contains(reference)));
    assert_eq!(revision.episode.lifecycle, before.lifecycle);
    assert_eq!(revision.episode.report, before.report);
    assert_eq!(revision.episode.recovery, before.recovery);
    let restored: DiagnosticRevision = serde_json::from_slice(&serde_json::to_vec(&revision).unwrap()).unwrap();
    assert_eq!(restored, revision);
}

#[test]
fn caller_constructed_stopped_events_cannot_omit_a_page_or_nonblocking_original_candidate() {
    let (intent, before, _, _) = writer_intent();
    let DiagnosticFocus::PageWaitWriters { focus } = &intent.focus else { panic!() };
    let nonblocking =
        focus.candidates.iter().find(|candidate| candidate.key.session.id == "53").unwrap().parent.clone();
    let page = focus.page.clone();
    let mut next = before.clone();
    next.analysis = DiagnosticAnalysisStatus::Unresolved;
    next.issue = Some(DiagnosticAnalysisIssue::Interrupted);
    let event = DiagnosticInvestigationEvent {
        intent: Box::new(intent),
        attempt: None,
        source_work: SourceWorkState::NotStarted,
        outcome: DiagnosticInvestigationOutcome::Stopped { issue: DiagnosticAnalysisIssue::Interrupted },
    };
    let revision =
        DiagnosticRevision::with_investigation(&before, next, event, "2020-01-01T00:04:00Z").unwrap().unwrap();
    for omitted in [page, nonblocking] {
        let mut changed = revision.clone();
        let original_count = changed.inputs.len();
        changed.inputs.retain(|reference| *reference != omitted);
        assert_eq!(changed.inputs.len() + 1, original_count);
        changed.id = changed.expected_id().unwrap();
        assert!(!changed.is_valid(), "a matching event digest never substitutes for complete prerequisite input refs");
    }
}
