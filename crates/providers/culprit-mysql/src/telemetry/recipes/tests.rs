use super::*;
use culprit_core::{ConnectionSpec, Engine};

fn connector() -> Arc<Connector> {
    Arc::new(
        Connector::new(&ConnectionSpec {
            engine: Some(Engine::Mysql),
            host: "127.0.0.1".into(),
            port: Some(1),
            database: Some("shop".into()),
            user: Some("unused".into()),
            password: Some("unused".into()),
            ..Default::default()
        })
        .unwrap(),
    )
}
fn resource(kind: SharedResourceKind, engine: Engine) -> SharedResourceIdentity {
    SharedResourceIdentity {
        engine,
        kind,
        namespace: NativeIdentity::new("native-namespace"),
        native: NativeIdentity::new("native-resource"),
    }
}
fn request(registration: &Registration) -> CollectionRequest {
    let selection = match registration.acquisition.collection() {
        TelemetryCollection::Activity => CollectionSelection::Activity,
        TelemetryCollection::DeadlockHistory => CollectionSelection::DeadlockHistory {
            from: "2020-01-01T00:00:00Z".into(),
            until: "2020-01-01T01:00:00Z".into(),
        },
        TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestWork) => {
            CollectionSelection::CurrentRequestWork { focus: Box::new(crate::progress::test_support::original_focus()) }
        }
        TelemetryCollection::Snapshot(dataset) => CollectionSelection::Snapshot { dataset },
        _ => panic!("the catalog contains an unimplemented acquisition"),
    };
    CollectionRequest {
        subject: match registration.acquisition.subject() {
            RecipeSubject::Database => CollectionSubject::Database,
            RecipeSubject::Shared(kind) => CollectionSubject::Shared { resource: resource(kind, Engine::Mysql) },
        },
        selection,
        budget: CollectionBudget::default(),
    }
}

#[tokio::test]
async fn every_static_declaration_selects_one_real_native_constructor_without_opening_a_session() {
    let declarations = crate::telemetry_recipes();
    assert_eq!(declarations.len(), REGISTRATIONS.len());
    assert!(recipe_catalog_is_valid(&declarations));
    for (registration, descriptor) in REGISTRATIONS.iter().zip(&declarations) {
        let request = request(registration);
        request.validate().unwrap_or_else(|error| panic!("{}: {error}", descriptor.id));
        assert!(descriptor.accepts(&request));
        assert_eq!(descriptor.cost, RecipeCostQualification::Unqualified);
        assert_eq!(REGISTRATIONS.iter().filter(|candidate| candidate.accepts(&request)).count(), 1);
        let mut source = crate::telemetry::source(connector(), &request).unwrap();
        assert_eq!(source.query_count(), 0, "{}", descriptor.id);
        assert!(source.read_metrics().is_none_or(|metrics| metrics.active_tasks == 0));
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(source.query_count(), 0);
    }
}

#[test]
fn expanded_status_allowlist_revises_only_its_recipe_identity_without_qualifying_cost() {
    let declarations = declarations();
    let operational = declarations.iter().find(|recipe| recipe.id == "mysql.operational").unwrap();
    assert_eq!(operational.version, 2);
    assert_eq!(
        operational.selector,
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::Operational))
    );
    assert!(declarations.iter().filter(|recipe| recipe.id != "mysql.operational").all(|recipe| recipe.version == 1));
    assert!(declarations.iter().all(|recipe| recipe.cost == RecipeCostQualification::Unqualified));
}

#[test]
fn baseline_and_investigation_meaning_is_explicit_without_qualifying_native_cost() {
    let declarations = declarations();
    for id in ["mysql.activity", "mysql.statement-counters", "mysql.operational", "mysql.native-failures"] {
        assert_eq!(declarations.iter().find(|recipe| recipe.id == id).unwrap().purpose, RecipePurpose::Baseline);
    }
    for id in [
        "mysql.session-memory",
        "mysql.group-replication",
        "mysql.runtime-distributions",
        "mysql.tables",
        "mysql.statement-definitions",
        "mysql.current-request-work",
    ] {
        assert_eq!(declarations.iter().find(|recipe| recipe.id == id).unwrap().purpose, RecipePurpose::Investigation);
    }
    assert!(declarations.iter().all(|recipe| recipe.cost == RecipeCostQualification::Unqualified));
    assert!(declarations.iter().all(|recipe| !matches!(
        recipe.selector,
        RecipeSelector::FocusedPageWait | RecipeSelector::Collection(TelemetryCollection::Diagnostics)
    )));
}

#[test]
fn profiles_reuse_native_caps_and_preserve_every_stricter_caller_dimension() {
    let strict = CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_sql_bytes: 1024,
        max_plan_bytes: 1024,
        max_pages: 1,
        max_queries: 1,
        statement_timeout_ms: 1,
        total_timeout_ms: 1,
    };
    strict.validate().unwrap();
    for registration in REGISTRATIONS {
        let mut request = request(registration);
        request.budget = CollectionBudget::upper_limits();
        assert_eq!(registration.bounded_request(&request).budget, registration.descriptor().budget);
        request.budget = strict.clone();
        assert_eq!(registration.bounded_request(&request).budget, strict, "{}", registration.id);
    }
    let activity = declarations().into_iter().find(|recipe| recipe.id == "mysql.activity").unwrap();
    assert_eq!((activity.budget.max_queries, activity.budget.max_snapshot_rows), (20, 500));
    assert_eq!((activity.budget.statement_timeout_ms, activity.budget.total_timeout_ms), (1000, 5000));
    let distribution = declarations().into_iter().find(|recipe| recipe.id == "mysql.runtime-distributions").unwrap();
    assert_eq!((distribution.budget.max_queries, distribution.budget.total_timeout_ms), (24, 10_000));
    let group = declarations().into_iter().find(|recipe| recipe.id == "mysql.group-replication").unwrap();
    assert_eq!((group.budget.max_queries, group.budget.max_snapshot_rows), (23, 256));
    assert_eq!((group.budget.statement_timeout_ms, group.budget.total_timeout_ms), (1000, 5000));
    let current = declarations().into_iter().find(|recipe| recipe.id == "mysql.current-request-work").unwrap();
    assert_eq!(current.selector, RecipeSelector::CurrentRequestWork);
    assert_eq!((current.budget.max_queries, current.budget.max_snapshot_rows), (24, 1));
    assert_eq!((current.budget.statement_timeout_ms, current.budget.total_timeout_ms), (1000, 5000));
}

#[test]
fn ordinary_catalog_and_retained_history_routes_keep_valid_above_default_budgets() {
    let defaults = CollectionBudget::default();
    let larger = CollectionBudget {
        max_snapshot_rows: defaults.max_snapshot_rows + 1,
        max_queries: defaults.max_queries + 1,
        total_timeout_ms: defaults.total_timeout_ms + 1,
        max_sql_bytes: defaults.max_sql_bytes + 1,
        ..defaults
    };
    larger.validate().unwrap();
    for id in ["mysql.tables", "mysql.deadlock-history", "mysql.statement-counters"] {
        let registration = REGISTRATIONS.iter().find(|recipe| recipe.id == id).unwrap();
        let mut request = request(registration);
        request.budget = larger.clone();
        assert_eq!(registration.bounded_request(&request).budget, larger, "{id}");
    }
}

#[test]
fn shared_native_sources_never_fall_through_to_database_or_other_resource_routes() {
    for registration in REGISTRATIONS {
        let original = request(registration);
        let mut wrong = original.clone();
        wrong.subject = match original.subject {
            CollectionSubject::Database => {
                CollectionSubject::Shared { resource: resource(SharedResourceKind::Instance, Engine::Mysql) }
            }
            CollectionSubject::Shared { .. } => CollectionSubject::Database,
        };
        assert!(crate::telemetry::source(connector(), &wrong).is_err(), "{}", registration.id);
    }
    let registration = REGISTRATIONS.iter().find(|recipe| recipe.id == "mysql.native-failures").unwrap();
    for shared in
        [resource(SharedResourceKind::Pool, Engine::Mysql), resource(SharedResourceKind::Instance, Engine::Postgres)]
    {
        let mut wrong = request(registration);
        wrong.subject = CollectionSubject::Shared { resource: shared };
        assert!(crate::telemetry::source(connector(), &wrong).is_err());
    }
}

#[test]
fn unsupported_native_selections_are_explicit_and_never_construct_a_fallback() {
    for selection in [
        CollectionSelection::QueryHistory { from: "2020-01-01T00:00:00Z".into(), until: "2020-01-01T01:00:00Z".into() },
        CollectionSelection::ResourceHistory {
            from: "2020-01-01T00:00:00Z".into(),
            until: "2020-01-01T01:00:00Z".into(),
        },
        CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets },
        CollectionSelection::Snapshot { dataset: SnapshotDataset::MemoryFailures },
    ] {
        let request = CollectionRequest { subject: CollectionSubject::Database, selection, budget: Default::default() };
        request.validate().unwrap();
        assert!(REGISTRATIONS.iter().all(|recipe| !recipe.accepts(&request)));
        let error = crate::telemetry::source(connector(), &request).err().unwrap();
        assert_eq!(error.code, ErrorCode::Unsupported);
    }
}

#[test]
fn current_request_work_is_never_admitted_as_an_unfocused_periodic_snapshot() {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestWork },
        budget: CollectionBudget::default(),
    };
    assert!(request.validate().is_err());
    assert!(REGISTRATIONS.iter().all(|recipe| !recipe.accepts(&request)));
    assert!(crate::telemetry::source(connector(), &request).is_err());
}

#[test]
fn current_request_work_requires_its_database_subject_and_exact_source_statement_token() {
    let registration = REGISTRATIONS.iter().find(|recipe| recipe.id == "mysql.current-request-work").unwrap();
    let valid = request(registration);
    valid.validate().unwrap();
    for wrong in [
        {
            let mut request = valid.clone();
            request.subject =
                CollectionSubject::Shared { resource: resource(SharedResourceKind::Instance, Engine::Mysql) };
            request
        },
        {
            let mut request = valid.clone();
            let CollectionSelection::CurrentRequestWork { focus } = &mut request.selection else { panic!() };
            focus.statement_identity = NativeIdentity::new("same-query-hash");
            request
        },
        {
            let mut request = valid.clone();
            let CollectionSelection::CurrentRequestWork { focus } = &mut request.selection else { panic!() };
            **focus = crate::progress::test_support::original_focus_in(SourcePartition::Target);
            focus.target =
                culprit_core::DatabaseIdentity::server(Engine::Postgres, "shop", "fixture.invalid", 5432, None).into();
            assert!(focus.is_valid(), "a structurally valid other-engine focus is not a MySQL event selector");
            request
        },
    ] {
        assert!(wrong.validate().is_err());
        assert!(
            crate::telemetry::source(connector(), &wrong).is_err(),
            "invalid selection cannot construct a native reader"
        );
    }
}
