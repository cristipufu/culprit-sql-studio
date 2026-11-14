use super::*;
use crate::{NewConnection, Workspace};
use culprit_core::{AuthKind, Engine, ErrorCode, telemetry::*};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn recipe(id: &str) -> RecipeDescriptor {
    RecipeDescriptor {
        id: id.into(),
        version: 1,
        purpose: RecipePurpose::Baseline,
        selector: RecipeSelector::Collection(TelemetryCollection::Activity),
        subjects: vec![RecipeSubject::Database],
        budget: CollectionBudget::default(),
        cost: RecipeCostQualification::Unqualified,
    }
}
fn registration(calls: Arc<AtomicUsize>, recipes: Vec<RecipeDescriptor>) -> Result<ProviderRegistration> {
    ProviderRegistration::new(
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Error::internal("selected provider factory reached"))
        }),
        recipes,
    )
}
fn saved(workspace: &Workspace, engine: Engine) {
    workspace
        .add_connection(NewConnection {
            name: "metadata".into(),
            input: "postgres://reader@fixture.invalid/shop".into(),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    let mut stored = workspace.store.get("metadata").unwrap();
    stored.engine = engine;
    workspace.store.put("metadata", stored).unwrap();
}

#[test]
fn runtime_registration_and_replacement_never_inherit_builtin_catalogs_for_any_engine() {
    for engine in Engine::ALL {
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let declared = recipe("external.baseline");
        let workspace = Workspace::open_in(directory.path())
            .with_provider(engine, registration(calls.clone(), vec![declared.clone()]).unwrap());
        saved(&workspace, engine);
        let mut listed = workspace.telemetry_recipes("metadata").unwrap();
        assert_eq!(listed, vec![declared.clone()]);
        listed[0].id = "caller-modified".into();
        assert_eq!(workspace.telemetry_recipes("metadata").unwrap(), vec![declared]);
        let workspace = workspace.with_provider(engine, registration(calls.clone(), Vec::new()).unwrap());
        assert!(
            workspace.telemetry_recipes("metadata").unwrap().is_empty(),
            "{engine:?}: empty override is undeclared, not built-in fallback"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn metadata_and_native_construction_select_the_same_bound_registration() {
    let directory = tempfile::tempdir().unwrap();
    let old = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let workspace = Workspace::open_in(directory.path())
        .with_provider(Engine::Postgres, registration(old.clone(), vec![recipe("external.old")]).unwrap())
        .with_provider(Engine::Postgres, registration(current.clone(), vec![recipe("external.current")]).unwrap());
    saved(&workspace, Engine::Postgres);
    assert_eq!(workspace.telemetry_recipes("metadata").unwrap()[0].id, "external.current");
    assert_eq!((old.load(Ordering::SeqCst), current.load(Ordering::SeqCst)), (0, 0));
    let error = workspace.connection_service().provider("metadata").await.err().unwrap();
    assert_eq!(error.message, "selected provider factory reached");
    assert_eq!((old.load(Ordering::SeqCst), current.load(Ordering::SeqCst)), (0, 1));
}

#[test]
fn invalid_catalogs_and_missing_routes_fail_without_native_factory_calls() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut invalid = recipe("external.invalid");
    invalid.version = 0;
    for recipes in [vec![invalid], vec![recipe("external.duplicate"), recipe("external.duplicate")]] {
        let error = registration(calls.clone(), recipes).err().unwrap();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert_eq!(error.message, culprit_core::t!("engine.registry.invalid_recipes"));
    }
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::open_in(directory.path())
        .with_provider(Engine::Postgres, registration(calls.clone(), vec![]).unwrap());
    assert_eq!(workspace.telemetry_recipes("missing").unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
