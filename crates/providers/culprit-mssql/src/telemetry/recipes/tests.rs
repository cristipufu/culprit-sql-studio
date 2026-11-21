use super::*;
use crate::read::ReadSession;
use std::collections::BTreeSet;

#[test]
fn registered_source_versions_include_native_acquisition_policy_revisions() {
    let recipes = declarations();
    let revised = recipes
        .iter()
        .filter(|recipe| recipe.version != 1)
        .map(|recipe| {
            assert_eq!(recipe.cost, RecipeCostQualification::Unqualified, "{}", recipe.id);
            (recipe.id.as_str(), recipe.version)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        revised,
        BTreeSet::from([
            ("mssql.database-performance-counters", 2),
            ("mssql.suspect-pages", 2),
            ("mssql.agent-jobs", 2),
            ("mssql.hyperscale-log-components", 2),
            ("mssql.query-history", 6),
            ("mssql.resource-history", 3),
            ("mssql.pool-resource-history", 2),
            ("mssql.deadlock-history", 2),
            ("mssql.system-health-deadlocks", 2),
            ("mssql.activity", 6),
            ("mssql.page-wait-targets", 3),
            ("mssql.page-wait-tasks", 3),
            ("mssql.shared-tempdb-page-wait-targets", 4),
            ("mssql.focused-page-wait", 3),
            ("mssql.focused-page-task-holders", 3),
            ("mssql.focused-page-wait-writers", 4),
            ("mssql.focused-shared-tempdb-page-wait", 4),
            ("mssql.focused-compiler-admission", 3),
            ("mssql.focused-shared-compiler-admission", 3),
            ("mssql.current-request-memory", 5),
            ("mssql.focused-request-memory", 5),
            ("mssql.cache-entry-memory", 2),
            ("mssql.task-space", 4),
            ("mssql.cleanup", 2),
            ("mssql.columnstore-rowgroups", 2),
            ("mssql.cdc", 2),
            ("mssql.backup-history", 2),
            ("mssql.operational", 9),
            ("mssql.shared-operational", 11),
            ("mssql.memory-failures", 2),
            ("mssql.plan-cache-summary", 2),
            ("mssql.missing-indexes", 2),
            ("mssql.tuning", 3),
            ("mssql.wait-counters", 2),
            ("mssql.replication", 4),
            ("mssql.settings", 6),
            ("mssql.statement-grants", 2),
            ("mssql.index-operations", 2),
            ("mssql.tables", 8),
            ("mssql.foreign-keys", 2),
            ("mssql.columns", 2),
            ("mssql.statistics", 2),
            ("mssql.indexes", 5),
            ("mssql.check-constraints", 2),
            ("mssql.query-compilation", 4),
            ("mssql.cached-runtime", 3),
            ("mssql.objects", 2),
            ("mssql.triggers", 2),
            ("mssql.partitions", 2),
            ("mssql.cached-definitions", 6),
            ("mssql.retained-plans", 7),
        ])
    );
}

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
async fn selection(selector: RecipeSelector) -> CollectionSelection {
    let from = "2020-01-01T06:00:00Z".to_owned();
    let until = "2020-01-01T07:00:00Z".to_owned();
    match selector {
        RecipeSelector::ResourcePoolSettings => CollectionSelection::ResourcePoolSettings,
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
        RecipeSelector::PageWaitTasks => {
            let focus = crate::activity::page_targets::holders::test_focus().await.participant;
            CollectionSelection::PageWaitTasks {
                activity: focus.parent.capture_ref(),
                original_requests: vec![ActivityRequestSelector {
                    key: focus.original.key.clone(),
                    statement_identity: focus.statement_identity.clone(),
                }],
                original_context: Box::new(NativeReadContext {
                    target: focus.target,
                    partition: focus.partition,
                    observed_from: focus.original_observed_at.clone(),
                    observed_until: focus.original_observed_at,
                    database_writable: focus.database_writable,
                }),
            }
        }
        RecipeSelector::FocusedPageTaskHolders => CollectionSelection::PageTaskHolders {
            focus: Box::new(crate::activity::page_targets::holders::test_focus().await),
        },
        RecipeSelector::FocusedPageWaitWriters => CollectionSelection::PageWaitWriters {
            focus: Box::new(crate::activity::page_targets::writers::test_focus().await),
        },
        RecipeSelector::FocusedRequestMemory => CollectionSelection::FocusedRequestMemory {
            focus: Box::new(crate::activity::memory::focus::tests::original_focus()),
        },
        RecipeSelector::FocusedCompilationAdmission => CollectionSelection::FocusedCompilationAdmission {
            focus: Box::new(crate::activity::compiler_admission::tests::focus()),
        },
        RecipeSelector::MaintenanceProgress => {
            panic!("the SQL Server catalog does not declare maintenance progress")
        }
        RecipeSelector::CurrentRequestWork => {
            panic!("the SQL Server catalog does not declare current request work")
        }
        RecipeSelector::FocusedBlocking => {
            panic!("the SQL Server catalog does not declare focused blocking")
        }
        RecipeSelector::TableMemory => {
            CollectionSelection::TableMemory { target: Box::new(crate::table_memory::tests::selection()) }
        }
        RecipeSelector::FullTextPopulations => CollectionSelection::FullTextPopulations {
            target: Box::new(crate::full_text_populations::tests::selection()),
        },
        RecipeSelector::ColumnstoreRowgroups => CollectionSelection::ColumnstoreRowgroups {
            target: Box::new(crate::columnstore_rowgroups::tests::selection()),
        },
        RecipeSelector::CacheEntryMemory => {
            CollectionSelection::CacheEntryMemory { focus: Box::new(crate::cache_entry_memory::tests::focus().await) }
        }
        RecipeSelector::Collection(TelemetryCollection::Diagnostics) => {
            panic!("derived diagnostics are not a native source")
        }
    }
}

#[tokio::test]
async fn every_declared_recipe_uses_exactly_one_actual_constructor_without_native_work() {
    let declarations = declarations();
    assert!(!declarations.is_empty());
    let mut ids = BTreeSet::new();
    for (registration, descriptor) in REGISTRATIONS.iter().zip(&declarations) {
        assert!(descriptor.is_valid(), "{}", descriptor.id);
        assert!(ids.insert(&descriptor.id));
        assert_eq!(descriptor.cost, RecipeCostQualification::Unqualified);
        assert_eq!(descriptor.version, registration.version);
        for &kind in registration.subjects {
            let mut selection = selection(descriptor.selector).await;
            let subject = subject(kind);
            if let (CollectionSelection::FocusedPageWait { focus }, CollectionSubject::Shared { resource }) =
                (&mut selection, &subject)
            {
                // This constructor-only fixture names an original instance member; no native authority is acquired.
                let SourcePartition::InstanceLocal { member, .. } = &mut focus.participant_mut().partition else {
                    panic!("a shared follow-up requires the original native member")
                };
                *member = Some(resource.clone());
                selection = CollectionSelection::SharedPageWaitFocus {
                    focus: focus.clone(),
                    partition: SourcePartition::InstanceLocal {
                        member: Some(resource.clone()),
                        epoch: Some("native-shared-generation".into()),
                    },
                };
            }
            if let (
                CollectionSelection::FocusedCompilationAdmission { focus },
                CollectionSubject::Shared { resource },
            ) = (&mut selection, &subject)
            {
                // Constructor-only identity relation; no source has observed or authenticated these fixture names.
                let SourcePartition::InstanceLocal { member, .. } = &mut focus.partition else {
                    panic!("native member")
                };
                *member = Some(resource.clone());
                selection = CollectionSelection::SharedCompilationAdmissionFocus {
                    focus: focus.clone(),
                    partition: SourcePartition::InstanceLocal {
                        member: Some(resource.clone()),
                        epoch: Some("synthetic-shared-compiler-generation".into()),
                    },
                };
            }
            let request = CollectionRequest { subject, selection, budget: descriptor.budget.clone() };
            assert!(descriptor.accepts(&request));
            assert_eq!(REGISTRATIONS.iter().filter(|entry| entry.accepts(&request)).count(), 1, "{}", descriptor.id);
            let source =
                open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "fixture.invalid".into(), 1433)
                    .unwrap();
            assert_eq!(source.query_count(), 0, "{}", descriptor.id);
            assert!(source.read_metrics().is_none_or(|metrics| metrics.active_tasks == 0));
            // Drop owns an unused native session; the constructor has not acquired a connection.
            drop(source);
            let mut only_setup = request.clone();
            only_setup.budget.max_queries = 1;
            let pool = Arc::new(Pool::new(tiberius::Config::new(), None));
            let Err(error) = open(pool.clone(), &only_setup, "fixture.invalid".into(), 1433) else {
                panic!("{} accepted no allowance for a source query after owned setup", descriptor.id);
            };
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit, "{}", descriptor.id);
            assert_eq!(pool.source_shutdown(), SourceShutdown::Quiescent, "{}", descriptor.id);
        }
    }
}

#[tokio::test]
async fn a_focused_selector_can_never_fall_back_to_the_broad_page_population() {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: selection(RecipeSelector::FocusedPageWait).await,
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
    assert_eq!(
        budget.max_queries,
        crate::page_waits::bounded_budget(&CollectionBudget::default()).max_queries
            + crate::read::ReadSession::SESSION_SETUP_QUERIES,
    );
}

#[test]
fn unregistered_datasets_or_subjects_return_unsupported_without_a_native_connection() {
    for (dataset, kind) in [
        (SnapshotDataset::RuntimeDistributions, RecipeSubject::Database),
        (SnapshotDataset::StatementCounters, RecipeSubject::Database),
        (SnapshotDataset::MemoryFailures, RecipeSubject::Database),
        (SnapshotDataset::Operational, RecipeSubject::Shared(SharedResourceKind::Pool)),
        (SnapshotDataset::PageWaitTargets, RecipeSubject::Shared(SharedResourceKind::Pool)),
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
fn configured_system_health_remains_an_explicit_unqualified_shared_investigation_with_finite_client_fences() {
    let recipe = declarations().into_iter().find(|recipe| recipe.id == "mssql.system-health-deadlocks").unwrap();
    assert_eq!(recipe.subjects, [RecipeSubject::Shared(SharedResourceKind::Instance)]);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.budget.max_queries, 9, "eight reader queries and one possible session SET batch");
    assert_eq!(recipe.budget.max_snapshot_rows, 50);
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
}

#[test]
fn an_ordinary_writer_snapshot_without_original_selectors_is_rejected_before_a_factory_can_open() {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitWriters },
        budget: CollectionBudget::default(),
    };
    assert_eq!(CollectionStop::from_error(&request.validate().unwrap_err()), CollectionStop::InvalidPage);
    assert!(REGISTRATIONS.iter().all(|recipe| !recipe.accepts(&request)));
    let error = open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "fixture.invalid".into(), 1433)
        .err()
        .unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
}

#[test]
fn specialized_descriptors_take_their_envelopes_from_native_profile_owners() {
    let page = declarations().into_iter().find(|descriptor| descriptor.id == "mssql.page-wait-targets").unwrap();
    assert_eq!(page.budget, page_waits::bounded_budget(&CollectionBudget::upper_limits()));
    let activity = declarations().into_iter().find(|descriptor| descriptor.id == "mssql.activity").unwrap();
    assert_eq!(activity.budget, activity::bounded_budget(&CollectionBudget::upper_limits()));
    let oom = declarations().into_iter().find(|descriptor| descriptor.id == "mssql.memory-failures").unwrap();
    assert_eq!(oom.budget.max_queries, 9, "six native phases, two snapshot clocks and one possible session SET");
}

#[tokio::test]
async fn caller_defaults_do_not_silently_cap_an_explicit_larger_history_capture() {
    let recipe = REGISTRATIONS.iter().find(|recipe| recipe.id == "mssql.query-history").unwrap();
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: selection(recipe.selector).await,
        budget: CollectionBudget::upper_limits(),
    };
    assert!(request.validate().is_ok());
    assert!(request.budget.max_snapshot_rows > CollectionBudget::default().max_snapshot_rows);
    assert_eq!(recipe.bounded_request(&request).budget, request.budget);
    let source =
        open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "fixture.invalid".into(), 1433).unwrap();
    assert_eq!(source.query_count(), 0);
}

#[tokio::test]
async fn native_dispatch_enforces_profile_envelopes_and_every_stricter_caller_limit() {
    let recipe = REGISTRATIONS.iter().find(|recipe| recipe.id == "mssql.focused-page-wait").unwrap();
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: selection(recipe.selector).await,
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

#[tokio::test]
async fn native_resource_discovery_derives_its_routes_from_registrations_and_observed_platform() {
    let CollectionSubject::Shared { resource: instance } = subject(RecipeSubject::Shared(SharedResourceKind::Instance))
    else {
        panic!()
    };
    for edition in [2, 3, 4, crate::probe::ENGINE_EDITION_MANAGED_INSTANCE] {
        let collections = collections_for(&instance, edition);
        let mut expected = vec![
            TelemetryCollection::Snapshot(SnapshotDataset::Jobs),
            TelemetryCollection::DeadlockHistory,
            TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets),
            TelemetryCollection::Snapshot(SnapshotDataset::NumaNodes),
            TelemetryCollection::Snapshot(SnapshotDataset::Operational),
        ];
        if edition != crate::probe::ENGINE_EDITION_MANAGED_INSTANCE {
            expected.insert(3, TelemetryCollection::Snapshot(SnapshotDataset::ProcessCpu));
        }
        if edition == crate::probe::ENGINE_EDITION_MANAGED_INSTANCE {
            expected.insert(1, TelemetryCollection::ResourceHistory);
            expected.push(TelemetryCollection::Snapshot(SnapshotDataset::MemoryFailures));
        }
        assert_eq!(collections, expected, "native edition {edition}");
        for collection in collections {
            let request = CollectionRequest {
                subject: CollectionSubject::Shared { resource: instance.clone() },
                selection: match collection {
                    TelemetryCollection::Snapshot(dataset) => CollectionSelection::Snapshot { dataset },
                    TelemetryCollection::ResourceHistory => CollectionSelection::ResourceHistory {
                        from: "2020-01-01T06:00:00Z".into(),
                        until: "2020-01-01T07:00:00Z".into(),
                    },
                    TelemetryCollection::DeadlockHistory => CollectionSelection::DeadlockHistory {
                        from: "2020-01-01T06:00:00Z".into(),
                        until: "2020-01-01T07:00:00Z".into(),
                    },
                    _ => panic!("registered instance snapshots"),
                },
                budget: CollectionBudget::default(),
            };
            let registration = REGISTRATIONS.iter().find(|entry| entry.accepts(&request)).unwrap();
            assert_eq!(registration.descriptor().cost, RecipeCostQualification::Unqualified);
            let mut source = open(
                Arc::new(Pool::new(tiberius::Config::new(), None)),
                &request,
                "never-connect.fixture.invalid".into(),
                1433,
            )
            .unwrap();
            assert_eq!(source.query_count(), 0, "discovery is not native acquisition or approval");
            assert_eq!(source.close().await, SourceShutdown::Quiescent);
        }
    }
    let CollectionSubject::Shared { resource: pool } = subject(RecipeSubject::Shared(SharedResourceKind::Pool)) else {
        panic!()
    };
    assert_eq!(
        collections_for(&pool, crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE),
        vec![
            TelemetryCollection::ResourceHistory,
            TelemetryCollection::Snapshot(SnapshotDataset::MemoryFailures),
            TelemetryCollection::Snapshot(SnapshotDataset::WaitCounters),
            TelemetryCollection::Snapshot(SnapshotDataset::Settings)
        ]
    );
}

#[test]
fn hyperscale_component_source_is_independent_and_does_not_grant_automatic_production_admission() {
    let declarations = declarations();
    let recipe = declarations.iter().find(|recipe| recipe.id == "mssql.hyperscale-log-components").unwrap();
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.budget.max_queries, 11, "ten reader queries and one possible session SET batch");
    assert_eq!(
        recipe.selector,
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::HyperscaleLogComponents))
    );
}

#[test]
fn cleanup_recipe_keeps_its_local_three_family_envelope_separate_from_azure_operational() {
    let recipes = declarations();
    let cleanup = recipes.iter().find(|recipe| recipe.id == "mssql.cleanup").unwrap();
    assert_eq!(cleanup.version, 2);
    assert_eq!(cleanup.purpose, RecipePurpose::Investigation);
    assert_eq!(cleanup.cost, RecipeCostQualification::Unqualified);
    assert_eq!(cleanup.subjects, vec![RecipeSubject::Database]);
    assert_eq!(cleanup.budget.max_queries, super::super::cleanup::MAX_QUERIES);
    assert_eq!(cleanup.budget.max_snapshot_rows, 3);
    assert_eq!(cleanup.budget.page_rows, 3);
    assert_eq!(cleanup.budget.page_bytes, 32 * 1024);
    assert_eq!(cleanup.budget.total_bytes, 32 * 1024);
    assert_eq!(cleanup.budget.statement_timeout_ms, 1_000);
    assert_eq!(cleanup.budget.total_timeout_ms, 5_000);
    assert_ne!(cleanup.selector, recipes.iter().find(|recipe| recipe.id == "mssql.operational").unwrap().selector);
}

#[test]
fn cdc_recipe_is_manual_unqualified_and_keeps_its_own_paid_native_roster() {
    let recipes = declarations();
    let cdc = recipes.iter().find(|recipe| recipe.id == "mssql.cdc").unwrap();
    assert_eq!(cdc.version, 2);
    assert_eq!(cdc.purpose, RecipePurpose::Investigation);
    assert_eq!(cdc.cost, RecipeCostQualification::Unqualified);
    assert_eq!(cdc.subjects, vec![RecipeSubject::Database]);
    assert_eq!(cdc.selector, RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::Cdc)));
    assert_eq!(cdc.budget.max_queries, 13, "twelve reader queries and one possible session SET batch");
    assert_eq!(cdc.budget.max_snapshot_rows, 98);
    assert_eq!(cdc.budget.page_rows, 32);
    assert_eq!(cdc.budget.page_bytes, 64 * 1024);
    assert_eq!(cdc.budget.total_bytes, 256 * 1024);
    assert_eq!(cdc.budget.statement_timeout_ms, 1000);
    assert_eq!(cdc.budget.total_timeout_ms, 5000);
}

#[test]
fn backup_history_is_an_explicit_manual_profile_with_no_baseline_or_cost_approval() {
    let recipes = declarations();
    let recipe = recipes.iter().find(|recipe| recipe.id == "mssql.backup-history").unwrap();
    assert_eq!(recipe.version, 2);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, vec![RecipeSubject::Database]);
    assert_eq!(
        recipe.selector,
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::BackupHistory))
    );
    assert_eq!(recipe.budget.max_queries, 13, "twelve reader queries and one possible session SET batch");
    assert_eq!(recipe.budget.max_snapshot_rows, 64);
    assert_eq!(recipe.budget.page_rows, 16);
    assert_eq!(recipe.budget.page_bytes, 256 * 1024);
    assert_eq!(recipe.budget.total_bytes, 512 * 1024);
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
}

#[test]
fn selected_columnstore_rowgroups_recipe_requires_native_target_and_stays_manual_unqualified() {
    let recipe = declarations().into_iter().find(|recipe| recipe.id == "mssql.columnstore-rowgroups").unwrap();
    assert_eq!(recipe.version, 2);
    assert_eq!(recipe.selector, RecipeSelector::ColumnstoreRowgroups);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.budget.max_snapshot_rows, 128);
    assert_eq!(recipe.budget.max_queries, 12, "eleven reader queries and one possible session SET batch");
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::ColumnstoreRowgroups {
            target: Box::new(crate::columnstore_rowgroups::tests::selection()),
        },
        budget: CollectionBudget::default(),
    };
    let source =
        open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "original.invalid".into(), 1433).unwrap();
    assert_eq!(source.query_count(), 0);
    let broad = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::ColumnstoreRowgroups },
        ..request
    };
    assert!(open(Arc::new(Pool::new(tiberius::Config::new(), None)), &broad, "original.invalid".into(), 1433).is_err());
}

#[test]
fn settings_local_profile_advertises_only_the_existing_native_budget_without_cost_approval() {
    let recipe = declarations().into_iter().find(|recipe| recipe.id == "mssql.settings").unwrap();
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, [RecipeSubject::Database]);
    assert_eq!(recipe.budget, super::super::state::settings_budget(&CollectionBudget::upper_limits()));
}

#[test]
fn selected_database_performance_counters_are_manual_unqualified_and_bounded_independently_of_shared_total() {
    let recipe = declarations().into_iter().find(|recipe| recipe.id == "mssql.database-performance-counters").unwrap();
    assert_eq!(recipe.version, 2);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, [RecipeSubject::Database]);
    assert_eq!(
        recipe.selector,
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::DatabasePerformanceCounters))
    );
    assert_eq!(recipe.budget, super::super::state::local_point_budget(&CollectionBudget::upper_limits()));
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::DatabasePerformanceCounters },
        budget: CollectionBudget::default(),
    };
    let source =
        open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "original.invalid".into(), 1433).unwrap();
    assert_eq!(source.query_count(), 0);
}

#[test]
fn suspect_page_history_is_one_explicit_unqualified_local_point_without_operational_expansion() {
    let recipe = declarations().into_iter().find(|recipe| recipe.id == "mssql.suspect-pages").unwrap();
    assert_eq!(recipe.version, 2);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, [RecipeSubject::Database]);
    assert_eq!(
        recipe.selector,
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::SuspectPages))
    );
    assert_eq!(recipe.budget, super::super::state::local_point_budget(&CollectionBudget::upper_limits()));
    assert_eq!(super::super::point::families(SnapshotDataset::SuspectPages).unwrap().len(), 1);
    assert!(super::super::point::shared_families(SnapshotDataset::SuspectPages).is_err());
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::SuspectPages },
        budget: CollectionBudget::default(),
    };
    let source =
        open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "original.invalid".into(), 1433).unwrap();
    assert_eq!(source.query_count(), 0);
}

#[test]
fn selected_table_memory_recipe_uses_actual_bounded_factory_and_remains_manual_unqualified() {
    let recipe = declarations().into_iter().find(|recipe| recipe.id == "mssql.table-memory").unwrap();
    assert_eq!(recipe.version, 1);
    assert_eq!(recipe.selector, RecipeSelector::TableMemory);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.subjects, [RecipeSubject::Database]);
    assert_eq!(recipe.budget.max_queries, 12);
    assert_eq!(recipe.budget.max_snapshot_rows, 1);
    assert_eq!(recipe.budget.page_rows, 1);
    assert_eq!(recipe.budget.page_bytes, 64 * 1024);
    assert_eq!(recipe.budget.total_bytes, 64 * 1024);
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::TableMemory { target: Box::new(crate::table_memory::tests::selection()) },
        budget: CollectionBudget::default(),
    };
    let source =
        open(Arc::new(Pool::new(tiberius::Config::new(), None)), &request, "original.invalid".into(), 1433).unwrap();
    assert_eq!(source.query_count(), 0);
    let broad = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::TableMemory },
        ..request
    };
    assert!(open(Arc::new(Pool::new(tiberius::Config::new(), None)), &broad, "original.invalid".into(), 1433).is_err());
}

#[test]
fn pool_governance_settings_are_one_closed_investigation_recipe_with_a_total_including_setup() {
    let recipe = declarations().into_iter().find(|recipe| recipe.id == "mssql.resource-pool-settings").unwrap();
    assert_eq!(recipe.subjects, vec![RecipeSubject::Database, RecipeSubject::Shared(SharedResourceKind::Pool)]);
    assert_eq!(recipe.selector, RecipeSelector::ResourcePoolSettings);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.budget.max_queries, 9);
    assert_eq!(crate::read::ReadSession::budget_after_setup(recipe.budget.clone()).unwrap().max_queries, 8);
    let original =
        CollectionBudget { max_queries: 7, total_timeout_ms: 1500, statement_timeout_ms: 500, ..Default::default() };
    let bounded = state::pool_settings_budget(&original);
    assert_eq!(bounded.max_queries, 7);
    assert_eq!(bounded.total_timeout_ms, 1500);
    assert_eq!(bounded.statement_timeout_ms, 500);
}

#[test]
fn managed_instance_history_is_an_explicit_native_profile_with_a_single_total_including_setup() {
    let recipes = declarations();
    let recipe = recipes.iter().find(|recipe| recipe.id == "mssql.managed-instance-resource-history").unwrap();
    assert_eq!(recipe.version, 1);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, [RecipeSubject::Shared(SharedResourceKind::Instance)]);
    assert_eq!(recipe.selector, RecipeSelector::Collection(TelemetryCollection::ResourceHistory));
    assert_eq!(recipe.budget.max_queries, 12);
    assert_eq!(crate::read::ReadSession::budget_after_setup(recipe.budget.clone()).unwrap().max_queries, 11);
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
    assert_eq!(recipe.budget.max_snapshot_rows, 512);
    assert_eq!(recipe.budget.page_bytes, 128 * 1024);
    let baseline = recipes.iter().find(|recipe| recipe.id == "mssql.resource-history").unwrap();
    assert_eq!(baseline.version, 3);
    assert_eq!(baseline.subjects, [RecipeSubject::Database]);
}

#[test]
fn change_feed_recipe_is_independent_manual_unqualified_and_pays_one_setup_inside_the_total() {
    let recipe = declarations().into_iter().find(|r| r.id == "mssql.change-feed").unwrap();
    assert_eq!(recipe.version, 1);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, [RecipeSubject::Database]);
    assert_eq!(recipe.selector, RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::ChangeFeed)));
    assert_eq!(recipe.budget.max_queries, 13);
    assert_eq!(recipe.budget.max_snapshot_rows, 98);
    assert_eq!(recipe.budget.page_rows, 32);
    assert_eq!(recipe.budget.total_bytes, 256 * 1024);
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
}

#[test]
fn numa_node_recipe_is_one_explicit_shared_instance_profile_with_setup_inside_its_manual_unqualified_total() {
    let selector = RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::NumaNodes));
    let recipes = declarations();
    let selected = recipes.iter().filter(|recipe| recipe.selector == selector).collect::<Vec<_>>();
    assert_eq!(selected.len(), 1);
    let recipe = selected[0];
    assert_eq!(recipe.id, "mssql.numa-nodes");
    assert_eq!(recipe.version, 1);
    assert_eq!(recipe.subjects, [RecipeSubject::Shared(SharedResourceKind::Instance)]);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.budget.max_queries, 11);
    assert_eq!(crate::read::ReadSession::budget_after_setup(recipe.budget.clone()).unwrap().max_queries, 10);
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
    assert_eq!(recipe.budget.max_snapshot_rows, 500);
    assert_eq!(recipe.budget.page_bytes, 256 * 1024);
    assert_eq!(recipe.budget.total_bytes, 4 * 1024 * 1024);
    let ordinary = recipes.iter().find(|recipe| recipe.id == "mssql.shared-operational").unwrap();
    assert_eq!(
        ordinary.selector,
        RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::Operational))
    );
}

#[test]
fn pool_current_wait_recipe_is_closed_bounded_unqualified_and_distinct_from_database_baseline() {
    let all = declarations();
    let recipe = all.iter().find(|recipe| recipe.id == "mssql.pool-wait-counters").unwrap();
    assert_eq!(recipe.version, 1);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, vec![RecipeSubject::Shared(SharedResourceKind::Pool)]);
    assert_eq!(recipe.budget.max_queries, 8);
    assert_eq!(recipe.budget.max_snapshot_rows, 500);
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
    let database = all.iter().find(|recipe| recipe.id == "mssql.wait-counters").unwrap();
    assert_eq!(database.subjects, vec![RecipeSubject::Database]);
}

#[test]
fn boxed_cpu_recipe_is_explicit_shared_instance_only_and_reserves_setup_inside_ten() {
    let selector = RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::ProcessCpu));
    let descriptors = declarations();
    let descriptor = descriptors.iter().find(|recipe| recipe.selector == selector).unwrap();
    assert_eq!(descriptor.purpose, RecipePurpose::Investigation);
    assert_eq!(descriptor.cost, RecipeCostQualification::Unqualified);
    for edition in [2, 3, 4] {
        assert!(
            collections_for(
                &match subject(RecipeSubject::Shared(SharedResourceKind::Instance)) {
                    CollectionSubject::Shared { resource } => resource,
                    _ => panic!(),
                },
                edition
            )
            .contains(&TelemetryCollection::Snapshot(SnapshotDataset::ProcessCpu))
        );
    }
    let caller = CollectionBudget::upper_limits();
    let bounded = state::process_cpu_budget(&caller);
    assert_eq!(bounded.max_queries, 10);
    assert_eq!(bounded.max_snapshot_rows, 69);
    assert_eq!(bounded.page_bytes, 64 * 1024);
    assert_eq!(bounded.total_bytes, 256 * 1024);
    assert_eq!(ReadSession::budget_after_setup(bounded).unwrap().max_queries, 9);
}
