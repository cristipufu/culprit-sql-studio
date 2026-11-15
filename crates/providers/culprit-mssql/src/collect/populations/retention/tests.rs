use super::*;

fn shared() -> Box<Snapshot> {
    let mut snapshot: Box<Snapshot> =
        serde_json::from_str(include_str!("../../../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.options = Some(SnapshotOptions::default());
    snapshot.workload = None;
    snapshot.notes.clear();
    // Native shared context remains useful in the requested report but must not multiply without a total limit.
    snapshot.tables[0].name = "private-shared-object-".repeat(1_024);
    snapshot.sources = vec![culprit_core::SourceOutcome {
        source: sources::TABLES.name.into(),
        state: culprit_core::SourceState::Collected,
        reason: None,
        reason_message: None,
    }];
    snapshot
}

fn workloads(catalog: &RuntimePopulationCatalog) -> Workloads {
    Workloads {
        requested: failed_part(&CollectionStop::ByteLimit.error(), Some(catalog.clone())),
        populations: catalog
            .populations
            .iter()
            .map(|population| {
                (population.identity.clone(), failed_part(&CollectionStop::ByteLimit.error(), Some(catalog.clone())))
            })
            .collect(),
        catalog: Some(catalog.clone()),
        stop_followup: false,
        pending_compilation: None,
        compilation: Default::default(),
    }
}

fn catalog() -> RuntimePopulationCatalog {
    crate::query_store::replicas::Catalog::decode(&[
        crate::query_store::replicas::tests::row(1, 1, None),
        crate::query_store::replicas::tests::row(7, 2, None),
    ])
    .unwrap()
    .population_catalog()
    .unwrap()
}

#[test]
fn an_output_limit_preserves_every_selector_but_never_leaves_omitted_catalog_data_healthy_empty() {
    let catalog = catalog();
    let mut shared = shared();
    shared.runtime_populations = Some(catalog.clone());
    let shared_name = shared.tables[0].name.clone();
    let common = serialized_size(&shared, MAX_POPULATION_SNAPSHOT_BYTES).unwrap();
    let limit = common * 2;
    let options = shared.options.clone().unwrap();
    let batch = project_with_limit(*shared, workloads(&catalog), &options, limit).unwrap();
    assert_eq!(batch.requested.tables[0].name, shared_name, "the requested native capture retains its shared facts");
    assert_eq!(batch.populations.len(), 2);
    assert!(serialized_size(&batch, limit).unwrap() <= limit);
    for (index, snapshot) in batch.populations.iter().enumerate() {
        assert_eq!(
            snapshot.options.as_ref().unwrap().workload_population.as_ref(),
            Some(&catalog.populations[index].identity)
        );
        assert_eq!(snapshot.runtime_populations.as_ref(), Some(&catalog));
        if snapshot.tables.is_empty() {
            assert!(snapshot.server.capabilities.is_empty());
            assert!(snapshot.sources.iter().all(|source| source.state == culprit_core::SourceState::Unavailable));
            assert!(snapshot.operations.index_usage_scope.is_none());
            assert!(!serde_json::to_string(snapshot).unwrap().contains(&shared_name));
        }
    }
    assert!(
        batch.populations.iter().any(|snapshot| snapshot.tables.is_empty()),
        "the total limit must exercise real omission"
    );
}

#[test]
fn a_missing_native_workload_still_retains_catalog_and_independent_default_cache_semantics() {
    let catalog = catalog();
    let shared = shared();
    let options = shared.options.clone().unwrap();
    let mut workloads = workloads(&catalog);
    let original: Box<Snapshot> =
        serde_json::from_str(include_str!("../../../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut cache = original.workload.unwrap();
    cache.kind = Some(WorkloadKind::PlanCache);
    cache.source = "Plan cache".into();
    cache.windowed = false;
    cache.window_start = None;
    cache.window_end = None;
    cache.population = None;
    cache.population_catalog = None;
    workloads.requested.workload = Some(cache);
    let batch = project(*shared, workloads, &options).unwrap();
    assert_eq!(batch.catalog.as_ref(), Some(&catalog));
    let cache = batch.requested.workload.as_ref().unwrap();
    assert_eq!(cache.kind, Some(WorkloadKind::PlanCache));
    assert!(cache.population.is_none() && cache.population_catalog.is_none());
    assert!(batch.populations.iter().all(|snapshot| snapshot.workload.is_none()));
    assert!(
        batch.populations.iter().all(|snapshot| snapshot.waits.is_empty()),
        "failed exact groups never inherit connected-member wait totals"
    );
}

#[test]
fn retained_workloads_share_one_budget_and_bounded_serialization_stops_without_building_an_output_buffer() {
    let original: Box<Snapshot> =
        serde_json::from_str(include_str!("../../../../../../../fixtures/mssql-demo.json")).unwrap();
    let workload = original.workload.unwrap();
    let statements: std::collections::HashMap<String, crate::query_store::costs::CompilationStatement> =
        Default::default();
    let bytes = serialized_size(&(&workload, &statements), MAX_RETAINED_WORKLOAD_BYTES).unwrap();
    let mut budget = WorkloadBudget { remaining: bytes, exhausted: false };
    budget.retain(&workload, &Default::default()).unwrap();
    let error = budget.retain(&workload, &Default::default()).unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::ByteLimit);
    assert!(budget.exhausted());
    assert_eq!(
        CollectionStop::from_error(&serialized_size(&(&workload, &statements), bytes - 1).unwrap_err()),
        CollectionStop::ByteLimit
    );
}
