use super::*;

fn recipe() -> RecipeDescriptor {
    RecipeDescriptor {
        id: "fixture.focused-page-wait".into(),
        version: 1,
        purpose: RecipePurpose::Investigation,
        selector: RecipeSelector::FocusedPageWait,
        subjects: vec![RecipeSubject::Database],
        budget: CollectionBudget::default(),
        cost: RecipeCostQualification::Unqualified,
    }
}

fn policies(recipes: Vec<RecipeDescriptor>) -> DiagnosticPolicies {
    DiagnosticPolicies::for_activity("fixture", CollectionSubject::Database, ActivityPolicy::default())
        .unwrap()
        .with_recipes("fixture", recipes)
        .unwrap()
}

#[tokio::test]
async fn an_unqualified_native_recipe_defers_the_question_without_reopening_the_source() {
    let mut harness = Harness::new().await;
    harness.policies = policies(vec![recipe()]);
    let native = harness.capture(baseline(0, vec![participant()])).await;
    let native_calls = harness.calls.counts();
    assert_eq!(harness.work(1).await, 1);
    assert_eq!(harness.calls.counts(), native_calls);
    let episode = harness.checkpoint().await.episodes.remove(0);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::CostUnqualified));
    assert_eq!(episode.first, harness.focus(&native).await.parent);
    assert_eq!(episode.latest, episode.first);
    assert_eq!(episode.revision, "1");
    assert!(episode.report.is_none());
    let revisions = harness.revisions(&native).await;
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].episode, episode);
    assert_eq!(harness.work(1).await, 0, "a deferral cannot requeue a sealed baseline");
}

#[tokio::test]
async fn recovery_and_restart_keep_deferred_analysis_and_the_original_native_receipt() {
    let mut harness = Harness::new().await;
    harness.policies = policies(vec![recipe()]);
    harness.capture(baseline(0, vec![participant()])).await;
    assert_eq!(harness.work(1).await, 1);
    let first = harness.checkpoint().await.episodes.remove(0);
    let recovered = harness.capture(baseline(10, vec![])).await;
    assert_eq!(harness.work(1).await, 1);
    let checkpoint = harness.checkpoint().await;
    let episode = &checkpoint.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
    assert_eq!(episode.issue, first.issue);
    assert_eq!(episode.first, first.first);
    assert_eq!(episode.latest, first.latest);
    assert_eq!(episode.revision, "2");
    assert_eq!(harness.revisions(&recovered).await[0].episode, *episode);
    let calls = harness.calls.counts();
    let count = harness.outbox.status(50, None).await.unwrap().captures;
    drop(harness.outbox);
    let mut outbox = Outbox::open(&harness.path).unwrap();
    outbox.recover().await.unwrap();
    assert_eq!(
        analyze_pending(&mut outbox, &harness.policies, &CollectionCancellation::default(), 10, None).await.unwrap(),
        0
    );
    assert_eq!(outbox.diagnostic_checkpoints(1, None, 10).await.unwrap().checkpoints, vec![checkpoint]);
    assert_eq!(outbox.status(50, None).await.unwrap().captures, count);
    assert!(outbox.diagnostic_evidence(&first.first).await.is_ok());
    assert_eq!(harness.calls.counts(), calls);
}

#[tokio::test]
async fn undeclared_or_inapplicable_catalogs_never_borrow_another_recipe() {
    let mut wrong_selector = recipe();
    wrong_selector.selector = RecipeSelector::Collection(TelemetryCollection::Activity);
    let mut wrong_purpose = recipe();
    wrong_purpose.purpose = RecipePurpose::Baseline;
    let mut wrong_subject = recipe();
    wrong_subject.subjects = vec![RecipeSubject::Shared(SharedResourceKind::Pool)];
    for catalog in [vec![], vec![wrong_selector], vec![wrong_purpose], vec![wrong_subject]] {
        let mut harness = Harness::new().await;
        harness.policies = policies(catalog);
        harness.capture(baseline(0, vec![participant()])).await;
        assert_eq!(harness.work(1).await, 1);
        let episode = harness.checkpoint().await.episodes.remove(0);
        assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Deferred);
        assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::Unsupported));
        assert!(episode.retained_in_checkpoint());
        assert_eq!(harness.calls.counts(), [1, 1, 1, 1]);
    }
}

#[tokio::test]
async fn recipe_policy_is_part_of_the_durable_step_identity() {
    let mut harness = Harness::new().await;
    harness.capture(baseline(0, vec![participant()])).await;
    let input = harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap();
    let at = clock(1);
    let mut unqualified = analyze(&input, &ActivityPolicy::default(), &at).unwrap();
    let available = policies(vec![recipe()]);
    crate::diagnostics::planning::qualify_questions(&input, available.assignment(&input), &mut unqualified).unwrap();
    let mut unsupported = analyze(&input, &ActivityPolicy::default(), &at).unwrap();
    let absent = policies(vec![]);
    crate::diagnostics::planning::qualify_questions(&input, absent.assignment(&input), &mut unsupported).unwrap();
    assert_ne!(unqualified.step_id, unsupported.step_id);
    assert_ne!(unqualified.checkpoint, unsupported.checkpoint);
    assert_eq!(unqualified.revisions[0].episode.revision, "1");
    assert_eq!(unsupported.revisions[0].episode.revision, "1");
    let mut replay = analyze(&input, &ActivityPolicy::default(), &clock(2)).unwrap();
    crate::diagnostics::planning::qualify_questions(&input, available.assignment(&input), &mut replay).unwrap();
    assert_eq!(replay.step_id, unqualified.step_id);
    assert_eq!(replay.revisions[0].id, unqualified.revisions[0].id);
    harness.outbox.commit_diagnostics(input, unqualified).await.unwrap();
    assert_eq!(harness.calls.counts(), [1, 1, 1, 1]);
}

#[tokio::test]
async fn equivalent_catalog_permutations_replay_the_same_step_but_changed_envelopes_do_not() {
    let mut harness = Harness::new().await;
    harness.capture(baseline(0, vec![participant()])).await;
    let input = harness.outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().unwrap();
    let mut focused = recipe();
    focused.subjects.push(RecipeSubject::Shared(SharedResourceKind::Pool));
    let mut ordinary = recipe();
    ordinary.id = "fixture.baseline".into();
    ordinary.purpose = RecipePurpose::Baseline;
    ordinary.selector = RecipeSelector::Collection(TelemetryCollection::Activity);
    let original = vec![ordinary.clone(), focused.clone()];
    focused.subjects.reverse();
    let reordered = vec![focused, ordinary];
    let qualified = |catalog| {
        let policy = policies(catalog);
        let mut update = analyze(&input, &ActivityPolicy::default(), &clock(1)).unwrap();
        crate::diagnostics::planning::qualify_questions(&input, policy.assignment(&input), &mut update).unwrap();
        update
    };
    let expected = qualified(original.clone());
    let replay = qualified(reordered);
    assert_eq!(expected.step_id, replay.step_id);
    assert_eq!(expected.checkpoint, replay.checkpoint);
    assert_eq!(expected.revisions, replay.revisions);
    for change in ["version", "budget"] {
        let mut changed = original.clone();
        let focused = changed.iter_mut().find(|recipe| recipe.selector == RecipeSelector::FocusedPageWait).unwrap();
        if change == "version" {
            focused.version += 1;
        } else {
            focused.budget.max_queries -= 1;
        }
        let changed = qualified(changed);
        assert_eq!(changed.checkpoint, expected.checkpoint);
        assert_ne!(changed.step_id, expected.step_id, "{change} changes the admitted policy envelope");
    }
    assert_eq!(harness.calls.counts(), [1, 1, 1, 1]);
}

#[test]
fn invalid_catalogs_or_unknown_routes_are_rejected_without_native_work() {
    let mut invalid = recipe();
    invalid.version = 0;
    assert!(
        DiagnosticPolicies::for_activity("fixture", CollectionSubject::Database, ActivityPolicy::default())
            .unwrap()
            .with_recipes("fixture", vec![invalid])
            .is_err()
    );
    assert!(
        DiagnosticPolicies::for_activity("fixture", CollectionSubject::Database, ActivityPolicy::default())
            .unwrap()
            .with_recipes("another-route", vec![recipe()])
            .is_err()
    );
}
