use super::*;
use culprit_collector::{analytical::test_support::shared_investigation_fixture, capture::CollectionAdmissionWindow};

#[tokio::test]
async fn an_admitted_shared_question_requires_its_sealed_owner_before_route_credentials_or_allocation() {
    let root = tempfile::tempdir().unwrap();
    let fixture = shared_investigation_fixture();
    let CollectionTarget::Shared(member) = &fixture.owner_source.target else { panic!() };
    let (mut outbox, focus, native_calls) =
        retained_parent_on_member(&root.path().join("outbox"), Some(member.clone())).await;
    let input = outbox
        .diagnostic_input(&focus.participant().parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    let TelemetryRecord::Activity(original) = &input.rows[0].row.record else { panic!() };
    let update = observe_diagnostic_activity(
        &input.source,
        &input.parent,
        &[DiagnosticActivityRow { reference: &input.rows[0].reference, observation: original }],
        None,
        &ActivityPolicy::default(),
        &focus.participant().original_observed_at,
    )
    .unwrap();
    let mut matching = update.checkpoint.episodes.iter().filter(|episode| episode.matches_page_wait_focus(&focus));
    let episode = matching.next().expect("the original page-wait focus selects its exact native episode");
    assert!(matching.next().is_none(), "one original focus cannot select multiple native episodes");
    let recipe = &fixture.investigation.recipe;
    let RecipeCostQualification::Measured(receipt) = &recipe.cost else { panic!() };
    let DiagnosticExecutionBinding::Shared { owner } = &fixture.investigation.intent.execution else { panic!() };
    let intent = DiagnosticInvestigationIntent::new_shared(
        episode,
        recipe,
        focus.into(),
        &receipt.component,
        &CollectionBudget::default(),
        DiagnosticSharedOwner { source: &fixture.owner_source, row: &fixture.owner_record, reference: owner },
    )
    .unwrap();
    let request = CollectionRequest {
        subject: intent.subject(),
        selection: intent.selection().unwrap(),
        budget: intent.budget.clone(),
    };
    let admitted = FleetSourceAdmission {
        window: CollectionAdmissionWindow::new(tokio::time::Instant::now(), &request.budget).unwrap(),
        route_revision: Some("fixture-saved-route".into()),
        investigation: Some(intent),
    };
    let workspace_path = root.path().join("workspace");
    let workspace = Workspace::open_in(&workspace_path);
    let outcome = workspace
        .collect_into_outbox_admitted(
            "missing-route",
            request.clone(),
            &mut outbox,
            &CollectionCancellation::default(),
            &admitted,
        )
        .await;
    let error = outcome.result.unwrap_err();
    assert_eq!(
        error.detail,
        Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)),
        "a structurally matching owner is not an authenticated retained artifact"
    );
    assert_eq!(outcome.source_work, SourceWorkState::NotStarted);
    assert!(outcome.attempt.is_none());
    assert_no_workspace_work(&workspace, &workspace_path);
    assert_eq!(outbox.status(10, None).await.unwrap().captures, 1);
    assert_eq!(native_calls.load(Ordering::SeqCst), 2);

    let output = root.path().join("shared-focus-output");
    let error = workspace
        .collect_once("missing-route", request, &output, &CollectionCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)));
    assert!(!output.exists());
    assert_no_workspace_work(&workspace, &workspace_path);
}
