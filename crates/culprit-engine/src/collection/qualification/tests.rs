use super::*;
use crate::NewConnection;
use culprit_collector::analytical::test_support::{QualificationFixture, QualificationFixtureCase};
use culprit_core::{AuthKind, Engine, Error, RecipeQualificationState};
use std::{path::Path, sync::Arc};

fn registration(fixture: &QualificationFixture, verifier: bool) -> ProviderRegistration {
    let mut registration = ProviderRegistration::new(
        Arc::new(|_, _context| Err(Error::internal("ordinary provider must not run during qualification"))),
        crate::ProviderConstructionPolicy::DeferredNativeWork,
        vec![fixture.recipe.clone()],
    )
    .unwrap();
    if verifier {
        let policy = fixture.clone();
        registration = registration
            .with_qualification_authority(
                fixture.component.clone(),
                Arc::new(move |receipt, source, row| Ok(policy.verifies(receipt, source, row))),
            )
            .unwrap();
    }
    let source = fixture.clone();
    let issuer = fixture.clone();
    registration.with_native_qualification_issuer(
        Arc::new(move |spec, request| {
            assert_eq!(spec.engine, Some(Engine::Mssql));
            assert!(source.source.matches_request(request));
            Ok(source.telemetry_source())
        }),
        Arc::new(move |evidence| issuer.assess(evidence)),
    )
}
fn workspace(root: &Path, fixture: &QualificationFixture, verifier: bool) -> Workspace {
    Workspace::open_in(root).with_provider(Engine::Mssql, registration(fixture, verifier))
}
fn saved(workspace: &Workspace, name: &str) {
    workspace
        .add_connection(NewConnection {
            name: name.into(),
            input: "mssql://reader@fixture.invalid/sample".into(),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
}
fn measured(workspace: &Workspace, name: &str) -> bool {
    matches!(workspace.telemetry_recipes(name).unwrap()[0].cost, RecipeCostQualification::Measured(_))
}
async fn install(workspace: &Workspace, outbox: &mut Outbox, fixture: &QualificationFixture) {
    let report = workspace
        .qualify_recipe("measured", &fixture.recipe.id, fixture.request(), outbox, &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(report.state, RecipeQualificationState::Installed);
}

#[tokio::test]
async fn live_issuance_changes_only_the_original_saved_connection_and_restart_reauthenticates_the_same_artifact() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let ws = workspace(root.path(), &fixture, true);
    saved(&ws, "measured");
    saved(&ws, "same-engine");
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    let before = ws.connection_service().saved_route("measured").unwrap();
    install(&ws, &mut outbox, &fixture).await;
    assert!(fixture.closed());
    assert_eq!((fixture.opens(), fixture.assessments()), (1, 1));
    assert!(measured(&ws, "measured"));
    assert!(!measured(&ws, "same-engine"));
    assert!(ws.connection_service().require_current_route(&before).is_err());
    let approved = ws.connection_service().saved_route("measured").unwrap();
    let receipt = match &ws.telemetry_recipes("measured").unwrap()[0].cost {
        RecipeCostQualification::Measured(receipt) => receipt.as_ref().clone(),
        _ => panic!(),
    };
    assert_eq!(
        receipt.evidence.capture.observation_id,
        ws.recipe_qualification_status("measured", &outbox.delivery(), &CollectionCancellation::default())
            .await
            .unwrap()[0]
            .capture
            .as_ref()
            .unwrap()
            .observation_id
    );
    drop(ws);
    drop(outbox);
    let ws = workspace(root.path(), &fixture, true);
    assert!(!measured(&ws, "measured"), "a fresh workspace never trusts serialized receipts as an installed cache");
    let mut outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    outbox.recover().await.unwrap();
    ws.restore_qualifications(&outbox.delivery(), Default::default(), &CollectionCancellation::default())
        .await
        .unwrap();
    assert!(measured(&ws, "measured"));
    assert!(!measured(&ws, "same-engine"));
    assert_eq!(ws.connection_service().saved_route("measured").unwrap().revision, approved.revision);
    assert_eq!(fixture.opens(), 1, "cold activation performs no native/provider work");
}

#[tokio::test]
async fn revoke_between_verified_restore_and_cache_publish_fences_the_old_route_and_cannot_resurrect_after_reinstall() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let ws = workspace(root.path(), &fixture, true);
    saved(&ws, "measured");
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    install(&ws, &mut outbox, &fixture).await;
    let ready = ws
        .read_qualification_bindings(&outbox.delivery(), Default::default(), &CollectionCancellation::default())
        .await
        .unwrap();
    let old = ws.connection_service().saved_route("measured").unwrap();
    outbox.revoke_qualification("measured", &fixture.recipe.id).await.unwrap();
    ws.qualification_bindings.replace(ready).unwrap();
    assert!(!measured(&ws, "measured"));
    assert!(ws.connection_service().provider_at_route(&old).await.is_err());
    assert_eq!(fixture.opens(), 1);
    install(&ws, &mut outbox, &fixture).await;
    assert_ne!(ws.connection_service().saved_route("measured").unwrap().revision, old.revision);
    assert!(ws.connection_service().provider_at_route(&old).await.is_err());
    assert!(measured(&ws, "measured"));
}

#[tokio::test]
async fn changed_saved_route_or_selected_component_cannot_borrow_another_native_approval() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let ws = workspace(root.path(), &fixture, true);
    saved(&ws, "measured");
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    install(&ws, &mut outbox, &fixture).await;
    let original = ws.store.get("measured").unwrap();
    let mut stored = original.clone();
    stored.host = "changed-route.invalid".into();
    ws.store.put("measured", stored).unwrap();
    assert!(!measured(&ws, "measured"));
    ws.restore_qualifications(&outbox.delivery(), Default::default(), &CollectionCancellation::default())
        .await
        .unwrap();
    assert!(!measured(&ws, "measured"));
    let summary = ws
        .recipe_qualification_status("measured", &outbox.delivery(), &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(summary[0].state, RecipeQualificationState::Refused);
    assert!(summary[0].gaps.contains(&culprit_core::RecipeQualificationGap::ConnectionChanged));
    ws.store.put("measured", original).unwrap();
    let mut another = fixture.clone();
    another.component.revision = Some("another-native-build".into());
    let other = workspace(root.path(), &another, true);
    other
        .restore_qualifications(&outbox.delivery(), Default::default(), &CollectionCancellation::default())
        .await
        .unwrap();
    assert!(!measured(&other, "measured"));
    assert_eq!(fixture.opens(), 1);
}

#[tokio::test]
async fn removed_or_renamed_connections_still_allow_inspection_and_revoke_without_permanent_pins() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let ws = workspace(root.path(), &fixture, true);
    saved(&ws, "measured");
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    install(&ws, &mut outbox, &fixture).await;
    ws.rename_connection("measured", "renamed").unwrap();
    assert!(!measured(&ws, "renamed"));
    let summary = ws
        .recipe_qualification_status("measured", &outbox.delivery(), &CollectionCancellation::default())
        .await
        .unwrap();
    assert_eq!(summary[0].state, RecipeQualificationState::Refused);
    ws.remove_connection("renamed").unwrap();
    assert!(ws.revoke_recipe_qualification("measured", &fixture.recipe.id, &outbox.delivery()).await.unwrap());
    assert_eq!(
        ws.recipe_qualification_status("measured", &outbox.delivery(), &CollectionCancellation::default())
            .await
            .unwrap()[0]
            .state,
        RecipeQualificationState::Revoked
    );
}

#[tokio::test]
async fn trusted_issuer_without_an_independent_native_acceptance_policy_cannot_install() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let ws = workspace(root.path(), &fixture, false);
    saved(&ws, "measured");
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    assert!(
        ws.qualify_recipe(
            "measured",
            &fixture.recipe.id,
            fixture.request(),
            &mut outbox,
            &CollectionCancellation::default()
        )
        .await
        .is_err()
    );
    assert!(fixture.closed());
    assert!(!measured(&ws, "measured"));
    assert!(
        ws.recipe_qualification_status("measured", &outbox.delivery(), &CollectionCancellation::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn unconfirmed_qualification_preserves_durable_source_debt_and_cannot_dispatch_a_replacement() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Unconfirmed);
    let ws = workspace(root.path(), &fixture, true);
    saved(&ws, "measured");
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    let summary = ws
        .qualify_recipe(
            "measured",
            &fixture.recipe.id,
            fixture.request(),
            &mut outbox,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(summary.state, RecipeQualificationState::Refused);
    assert!(ws.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(!measured(&ws, "measured"));
    ws.revoke_recipe_qualification("measured", &fixture.recipe.id, &outbox.delivery()).await.unwrap();
    let error = ws
        .qualify_recipe(
            "measured",
            &fixture.recipe.id,
            fixture.request(),
            &mut outbox,
            &CollectionCancellation::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(fixture.opens(), 1);
    assert_eq!(fixture.assessments(), 0);
}

#[tokio::test]
async fn corrupted_original_or_cancelled_restore_withholds_the_complete_cache_without_new_native_work() {
    let root = tempfile::tempdir().unwrap();
    let fixture = QualificationFixture::new(QualificationFixtureCase::Approve);
    let ws = workspace(root.path(), &fixture, true);
    saved(&ws, "measured");
    let mut outbox = Outbox::create(&root.path().join("outbox"), Default::default()).unwrap();
    install(&ws, &mut outbox, &fixture).await;
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    assert_eq!(
        CollectionStop::from_error(
            &ws.restore_qualifications(&outbox.delivery(), Default::default(), &cancellation).await.unwrap_err()
        ),
        CollectionStop::Cancelled
    );
    assert!(!measured(&ws, "measured"));
    let id = ws
        .recipe_qualification_status("measured", &outbox.delivery(), &CollectionCancellation::default())
        .await
        .unwrap()[0]
        .capture
        .as_ref()
        .unwrap()
        .observation_id
        .clone();
    let body = root.path().join("outbox/captures").join(id).join("operational-samples.jsonl");
    let directory = body.parent().unwrap();
    let manifest: CollectionManifest =
        serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    std::fs::write(directory.join(&manifest.files[0].file), b"changed-original").unwrap();
    assert!(
        ws.restore_qualifications(&outbox.delivery(), Default::default(), &CollectionCancellation::default())
            .await
            .is_err()
    );
    assert!(!measured(&ws, "measured"));
    assert_eq!(fixture.opens(), 1);
    assert_eq!(outbox.status(10, None).await.unwrap().entries[0].state, OutboxCaptureState::Quarantined);
}
