use super::*;
use culprit_core::{ConnectionSpec, Engine};

fn connector() -> Connector {
    Connector::new(&ConnectionSpec {
        engine: Some(Engine::Postgres),
        host: "fixture.invalid".into(),
        port: Some(9),
        ..Default::default()
    })
    .unwrap()
}

fn subject(kind: RecipeSubject) -> CollectionSubject {
    match kind {
        RecipeSubject::Database => CollectionSubject::Database,
        RecipeSubject::Shared(kind) => CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: Engine::Postgres,
                kind,
                namespace: NativeIdentity::new("native-network"),
                native: NativeIdentity::new("native-instance"),
            },
        },
    }
}

fn request(registration: &Registration, budget: CollectionBudget) -> CollectionRequest {
    CollectionRequest {
        subject: subject(registration.subject),
        selection: match registration.route.collection() {
            TelemetryCollection::Activity => CollectionSelection::Activity,
            TelemetryCollection::Snapshot(dataset) => CollectionSelection::Snapshot { dataset },
            _ => panic!("the catalog contains only implemented current sources"),
        },
        budget,
    }
}

#[test]
fn every_public_declaration_has_one_actual_offline_constructor_and_no_native_cost_qualification() {
    let declarations = crate::telemetry_recipes();
    assert!(recipe_catalog_is_valid(&declarations));
    assert_eq!(declarations.len(), REGISTRATIONS.len());
    let connector = connector();
    for (registration, descriptor) in REGISTRATIONS.iter().zip(declarations) {
        let request = request(registration, CollectionBudget::default());
        assert_eq!(descriptor, registration.descriptor());
        assert_eq!(descriptor.cost, RecipeCostQualification::Unqualified);
        assert_eq!(descriptor.version, 1);
        assert!(descriptor.accepts(&request));
        assert_eq!(REGISTRATIONS.iter().filter(|route| route.accepts(&request)).count(), 1);
        let source = super::super::source(connector.clone(), &request).unwrap();
        assert_eq!(source.query_count(), 0, "{}", descriptor.id);
        assert!(source.read_metrics().is_none_or(|metrics| metrics.active_tasks == 0));
        drop(source);
        assert_eq!(connector.source_shutdown(), SourceShutdown::Quiescent);
    }
}

#[test]
fn unsupported_collections_and_subjects_cannot_construct_an_inherited_native_source() {
    let connector = connector();
    for (dataset, kind) in [
        (SnapshotDataset::PageWaitTargets, RecipeSubject::Database),
        (SnapshotDataset::RuntimeDistributions, RecipeSubject::Database),
        (SnapshotDataset::TaskSpace, RecipeSubject::Database),
        (SnapshotDataset::NativeFailures, RecipeSubject::Database),
        (SnapshotDataset::RetainedPlans, RecipeSubject::Database),
        (SnapshotDataset::StatementCounters, RecipeSubject::Shared(SharedResourceKind::Instance)),
        (SnapshotDataset::Operational, RecipeSubject::Shared(SharedResourceKind::Pool)),
    ] {
        let request = CollectionRequest {
            subject: subject(kind),
            selection: CollectionSelection::Snapshot { dataset },
            budget: CollectionBudget::default(),
        };
        assert!(REGISTRATIONS.iter().all(|route| !route.accepts(&request)));
        assert_eq!(open(connector.clone(), &request).err().unwrap().code, ErrorCode::Unsupported);
        assert_eq!(connector.source_shutdown(), SourceShutdown::Quiescent);
    }
    let mut wrong_engine = subject(RecipeSubject::Shared(SharedResourceKind::Instance));
    let CollectionSubject::Shared { resource } = &mut wrong_engine else { panic!() };
    resource.engine = Engine::Yugabyte;
    let request = CollectionRequest {
        subject: wrong_engine,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget::default(),
    };
    assert_eq!(open(connector, &request).err().unwrap().code, ErrorCode::Unsupported);
    assert!(declarations().iter().all(|recipe| recipe.selector != RecipeSelector::FocusedPageWait));
}

#[test]
fn activity_uses_its_existing_native_profile_while_generic_recipes_keep_larger_valid_caller_budgets() {
    let maximum = CollectionBudget::upper_limits();
    maximum.validate().unwrap();
    for registration in REGISTRATIONS {
        let request = request(registration, maximum.clone());
        let bounded = registration.bounded_request(&request);
        bounded.validate().unwrap();
        if matches!(registration.route, Route::Activity) {
            assert_eq!(bounded.budget, activity::bounded_budget(&maximum));
            assert_eq!(bounded.budget.max_queries, 4);
            assert_eq!(bounded.budget.max_snapshot_rows, 500);
            assert_eq!(bounded.budget.statement_timeout_ms, 1_000);
            assert_eq!(bounded.budget.total_timeout_ms, 5_000);
        } else {
            assert_eq!(bounded.budget, maximum, "{}", registration.id);
            assert!(bounded.budget.max_snapshot_rows > CollectionBudget::default().max_snapshot_rows);
        }
        assert_eq!(registration.descriptor().budget, bounded.budget);
        assert_eq!(open(connector(), &request).unwrap().query_count(), 0);
    }
}

#[test]
fn every_stricter_caller_dimension_survives_catalog_and_source_construction() {
    let strict = CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_sql_bytes: 1024,
        max_plan_bytes: 1024,
        max_pages: 1,
        max_queries: 2,
        statement_timeout_ms: 10,
        total_timeout_ms: 20,
    };
    strict.validate().unwrap();
    for registration in REGISTRATIONS {
        let request = request(registration, strict.clone());
        assert_eq!(registration.bounded_request(&request).budget, strict, "{}", registration.id);
        assert_eq!(open(connector(), &request).unwrap().query_count(), 0);
    }
}

#[test]
fn shared_resource_discovery_uses_the_same_native_subject_registrations() {
    let CollectionSubject::Shared { resource } = subject(RecipeSubject::Shared(SharedResourceKind::Instance)) else {
        panic!()
    };
    assert_eq!(collections_for(&resource), vec![TelemetryCollection::Snapshot(SnapshotDataset::Operational)]);
    for collection in collections_for(&resource) {
        assert!(crate::telemetry_recipes().iter().any(|recipe| {
            recipe.selector == RecipeSelector::Collection(collection)
                && recipe.subjects == vec![RecipeSubject::Shared(SharedResourceKind::Instance)]
        }));
    }
    let mut unsupported = resource.clone();
    unsupported.kind = SharedResourceKind::Pool;
    assert!(collections_for(&unsupported).is_empty());
    unsupported = resource;
    unsupported.engine = Engine::Yugabyte;
    assert!(collections_for(&unsupported).is_empty());
}
