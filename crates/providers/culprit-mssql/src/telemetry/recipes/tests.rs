use super::*;
use std::collections::BTreeSet;

fn subject(kind: RecipeSubject) -> CollectionSubject {
    match kind {
        RecipeSubject::Database => CollectionSubject::Database,
        RecipeSubject::Shared(kind) => CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind,
                namespace: NativeIdentity::new("native-namespace"),
                native: NativeIdentity::new("native-resource"),
            },
        },
    }
}
fn selection(selector: RecipeSelector) -> CollectionSelection {
    let from = "2020-01-01T06:00:00Z".to_owned();
    let until = "2020-01-01T07:00:00Z".to_owned();
    match selector {
        RecipeSelector::Collection(TelemetryCollection::QueryHistory) => {
            CollectionSelection::QueryHistory { from, until }
        }
        RecipeSelector::Collection(TelemetryCollection::ResourceHistory) => {
            CollectionSelection::ResourceHistory { from, until }
        }
        RecipeSelector::Collection(TelemetryCollection::DeadlockHistory) => {
            CollectionSelection::DeadlockHistory { from, until }
        }
        RecipeSelector::Collection(TelemetryCollection::Activity) => CollectionSelection::Activity,
        RecipeSelector::Collection(TelemetryCollection::Snapshot(dataset)) => CollectionSelection::Snapshot { dataset },
        RecipeSelector::FocusedPageWait => CollectionSelection::FocusedPageWait {
            focus: Box::new(crate::activity::page_targets::test_focus(
                &crate::activity::page_targets::tests::page_wait(42, "PAGELATCH_EX", "7:1:4144"),
            )),
        },
        RecipeSelector::Collection(TelemetryCollection::Diagnostics) => {
            panic!("derived diagnostics are not a native source")
        }
    }
}

#[test]
fn every_declared_recipe_uses_exactly_one_actual_constructor_without_native_work() {
    let declarations = declarations();
    assert!(!declarations.is_empty());
    let mut ids = BTreeSet::new();
    for (registration, descriptor) in REGISTRATIONS.iter().zip(&declarations) {
        assert!(descriptor.is_valid(), "{}", descriptor.id);
        assert!(ids.insert(&descriptor.id));
        assert_eq!(descriptor.cost, RecipeCostQualification::Unqualified);
        assert_eq!(descriptor.version, 1);
        for &kind in registration.subjects {
            let request = CollectionRequest {
                subject: subject(kind),
                selection: selection(descriptor.selector),
                budget: descriptor.budget.clone(),
            };
            assert!(descriptor.accepts(&request));
            assert_eq!(REGISTRATIONS.iter().filter(|entry| entry.accepts(&request)).count(), 1, "{}", descriptor.id);
            let source =
                open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "fixture.invalid".into(), 1433)
                    .unwrap();
            assert_eq!(source.query_count(), 0, "{}", descriptor.id);
            assert!(source.read_metrics().is_none_or(|metrics| metrics.active_tasks == 0));
            // Drop owns an unused native session; the constructor has not acquired a connection.
            drop(source);
        }
    }
}

#[test]
fn a_focused_selector_can_never_fall_back_to_the_broad_page_population() {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: selection(RecipeSelector::FocusedPageWait),
        budget: CollectionBudget::default(),
    };
    let selected = REGISTRATIONS.iter().filter(|recipe| recipe.accepts(&request)).collect::<Vec<_>>();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id, "mssql.focused-page-wait");
    assert_eq!(selected[0].purpose, RecipePurpose::Investigation);
    assert!(
        !RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets)).matches(&request)
    );
    let budget = selected[0].descriptor().budget;
    assert_eq!(budget.max_snapshot_rows, 1);
    assert_eq!(budget.page_rows, 1);
    assert_eq!(budget.max_queries, crate::page_waits::bounded_budget(&CollectionBudget::default()).max_queries);
}

#[test]
fn unregistered_datasets_or_subjects_return_unsupported_without_a_native_connection() {
    for (dataset, kind) in [
        (SnapshotDataset::RuntimeDistributions, RecipeSubject::Database),
        (SnapshotDataset::StatementCounters, RecipeSubject::Database),
        (SnapshotDataset::MemoryFailures, RecipeSubject::Database),
        (SnapshotDataset::Operational, RecipeSubject::Shared(SharedResourceKind::Pool)),
        (SnapshotDataset::PageWaitTargets, RecipeSubject::Shared(SharedResourceKind::Instance)),
    ] {
        let request = CollectionRequest {
            subject: subject(kind),
            selection: CollectionSelection::Snapshot { dataset },
            budget: CollectionBudget::default(),
        };
        assert!(REGISTRATIONS.iter().all(|recipe| !recipe.accepts(&request)));
        let error = open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "fixture.invalid".into(), 1433)
            .err()
            .unwrap();
        assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
    }
}

#[test]
fn specialized_descriptors_take_their_envelopes_from_native_profile_owners() {
    let page = declarations().into_iter().find(|descriptor| descriptor.id == "mssql.page-wait-targets").unwrap();
    assert_eq!(page.budget, crate::page_waits::bounded_budget(&CollectionBudget::default()));
    let activity = declarations().into_iter().find(|descriptor| descriptor.id == "mssql.activity").unwrap();
    assert_eq!(activity.budget, activity::bounded_budget(&CollectionBudget::default()));
    let oom = declarations().into_iter().find(|descriptor| descriptor.id == "mssql.memory-failures").unwrap();
    assert_eq!(oom.budget.max_queries, 8, "six native phases and two named snapshot-clock calls");
}

#[test]
fn native_dispatch_enforces_profile_envelopes_and_every_stricter_caller_limit() {
    let recipe = REGISTRATIONS.iter().find(|recipe| recipe.id == "mssql.focused-page-wait").unwrap();
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: selection(recipe.selector),
        budget: CollectionBudget {
            page_rows: 10_000,
            max_snapshot_rows: 100_000,
            page_bytes: 8 * 1024 * 1024,
            total_bytes: 1024 * 1024 * 1024,
            max_sql_bytes: 4 * 1024 * 1024,
            max_plan_bytes: 4 * 1024 * 1024,
            max_pages: 10_000,
            max_queries: 100_000,
            statement_timeout_ms: 60_000,
            total_timeout_ms: 900_000,
        },
    };
    assert_eq!(recipe.bounded_request(&request).budget, recipe.descriptor().budget);
    let mut strict = request;
    strict.budget = CollectionBudget {
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
    assert_eq!(recipe.bounded_request(&strict).budget, strict.budget);
    let source =
        open(Arc::new(Pool::new(tiberius::Config::new(), None)), &strict, "fixture.invalid".into(), 1433).unwrap();
    assert_eq!(source.query_count(), 0);
}

#[test]
fn native_resource_discovery_derives_its_routes_from_registrations_and_observed_platform() {
    let CollectionSubject::Shared { resource: instance } = subject(RecipeSubject::Shared(SharedResourceKind::Instance))
    else {
        panic!()
    };
    for edition in [3, crate::probe::ENGINE_EDITION_MANAGED_INSTANCE] {
        let collections = collections_for(&instance, edition);
        assert!(collections.contains(&TelemetryCollection::Snapshot(SnapshotDataset::Operational)));
        assert_eq!(
            collections.contains(&TelemetryCollection::Snapshot(SnapshotDataset::MemoryFailures)),
            edition == crate::probe::ENGINE_EDITION_MANAGED_INSTANCE
        );
        for collection in collections {
            assert!(REGISTRATIONS.iter().any(|entry| {
                entry.selector == RecipeSelector::Collection(collection)
                    && entry
                        .subjects
                        .iter()
                        .any(|kind| kind.accepts(&CollectionSubject::Shared { resource: instance.clone() }))
            }));
        }
    }
    let CollectionSubject::Shared { resource: pool } = subject(RecipeSubject::Shared(SharedResourceKind::Pool)) else {
        panic!()
    };
    assert_eq!(
        collections_for(&pool, crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE),
        vec![TelemetryCollection::ResourceHistory, TelemetryCollection::Snapshot(SnapshotDataset::MemoryFailures)]
    );
}
