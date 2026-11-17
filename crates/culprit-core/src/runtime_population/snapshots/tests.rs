use super::*;
use crate::{DatabaseIdentity, Engine, WorkloadKind, i18n::Message};

fn batch() -> PopulationSnapshots {
    let mut requested: Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    requested.bind_identity(DatabaseIdentity::server(
        Engine::Mssql,
        &requested.server.database,
        "localhost",
        1433,
        None,
    ));
    let catalog = RuntimePopulationCatalog {
        membership: crate::RuntimePopulationMembership::NativeCatalog,
        source: crate::source_id::QUERY_STORE.into(),
        partitioned: true,
        requires_selection: true,
        populations: ["1", "7"]
            .map(|id| RuntimePopulation {
                identity: NativeIdentity::new(id),
                label: Message::from("fixture population"),
                native_properties: Default::default(),
            })
            .into(),
    };
    let options = SnapshotOptions { window_minutes: 60, ..Default::default() };
    requested.options = Some(options.clone());
    requested.runtime_populations = Some(catalog.clone());
    let mut workload = requested.workload.take().unwrap();
    workload.kind = Some(WorkloadKind::QueryStore);
    workload.population_catalog = Some(catalog.clone());
    workload.window_start = Some("2026-10-11T11:00:00Z".into());
    workload.window_end = Some("2026-10-11T12:00:00Z".into());
    workload.window_hours = 1.0;
    let populations = catalog
        .populations
        .iter()
        .map(|population| {
            let mut snapshot = requested.clone();
            snapshot.options =
                Some(SnapshotOptions { workload_population: Some(population.identity.clone()), ..options.clone() });
            let mut workload = workload.clone();
            workload.population = Some(population.identity.clone());
            snapshot.workload = Some(workload);
            snapshot
        })
        .collect();
    PopulationSnapshots { requested, populations, catalog: Some(catalog) }
}

#[test]
fn exact_populations_share_one_database_and_native_window_without_a_default_aggregate() {
    let batch = batch();
    batch.validate(batch.requested.options.as_ref().unwrap()).unwrap();
    assert!(batch.requested.workload.is_none());
    let mut selected =
        PopulationSnapshots { requested: batch.populations[1].clone(), populations: vec![], catalog: batch.catalog };
    let options = selected.requested.options.clone().unwrap();
    selected.validate(&options).unwrap();
    selected.requested.workload = None;
    selected.validate(&options).unwrap();
    selected.requested.options.as_mut().unwrap().workload_population = Some(NativeIdentity::new("unknown"));
    assert!(selected.validate(selected.requested.options.as_ref().unwrap()).is_err());
}

#[test]
fn a_default_cannot_hide_a_pooled_source_behind_an_omitted_population() {
    let mut batch = batch();
    let options = batch.requested.options.clone().unwrap();
    let mut pooled = batch.populations[0].workload.clone().unwrap();
    pooled.population = None;
    batch.requested.workload = Some(pooled);
    assert!(batch.validate(&options).is_err());
    let fallback = batch.requested.workload.as_mut().unwrap();
    fallback.kind = Some(WorkloadKind::PlanCache);
    fallback.windowed = false;
    fallback.population_catalog = None;
    batch.validate(&options).unwrap();
}

#[test]
fn foreign_duplicate_malformed_or_incomplete_population_results_are_rejected() {
    for defect in ["foreign", "duplicate", "malformed", "missing", "options", "native-window"] {
        let mut batch = batch();
        let options = batch.requested.options.clone().unwrap();
        match defect {
            "foreign" => {
                batch.populations[0].bind_identity(DatabaseIdentity::server(
                    Engine::Mssql,
                    "other",
                    "localhost",
                    1433,
                    None,
                ));
            }
            "duplicate" => batch.populations[1] = batch.populations[0].clone(),
            "malformed" => batch.populations[0].workload.as_mut().unwrap().population = Some(NativeIdentity::new("")),
            "missing" => {
                batch.populations.pop();
            }
            "options" => batch.populations[0].options.as_mut().unwrap().collect_page_wait_targets = true,
            _ => batch.populations[0].workload.as_mut().unwrap().window_end = Some("2026-10-11T12:01:00Z".into()),
        }
        assert!(batch.validate(&options).is_err(), "{defect}");
    }
}

#[test]
fn the_requested_native_window_must_match_its_other_population_inputs() {
    let mut batch = batch();
    batch.catalog.as_mut().unwrap().requires_selection = false;
    for snapshot in std::iter::once(&mut batch.requested).chain(&mut batch.populations) {
        snapshot.runtime_populations = batch.catalog.clone();
        if let Some(workload) = &mut snapshot.workload {
            workload.population_catalog = batch.catalog.clone();
        }
    }
    batch.requested.workload = batch.populations[0].workload.clone();
    let options = batch.requested.options.clone().unwrap();
    batch.validate(&options).unwrap();
    batch.requested.workload.as_mut().unwrap().window_start = Some("2026-10-11T10:59:00Z".into());
    assert!(batch.validate(&options).is_err());
}

#[test]
fn observed_membership_is_bound_to_each_original_workload_window() {
    let mut input = batch();
    let membership = RuntimePopulationMembership::ObservedWindow {
        from: "2026-10-11T11:00:00Z".into(),
        until: "2026-10-11T12:00:00Z".into(),
    };
    input.catalog.as_mut().unwrap().membership = membership;
    for snapshot in std::iter::once(&mut input.requested).chain(&mut input.populations) {
        snapshot.runtime_populations = input.catalog.clone();
        if let Some(workload) = &mut snapshot.workload {
            workload.population_catalog = input.catalog.clone();
        }
    }
    let options = input.requested.options.clone().unwrap();
    input.validate(&options).unwrap();
    assert!(!input.catalog.as_ref().unwrap().has_complete_membership());
    // Even a caller that omits the redundant workload catalog cannot borrow membership from another window.
    input.populations[0].workload.as_mut().unwrap().population_catalog = None;
    input.populations[0].workload.as_mut().unwrap().window_start = Some("2026-10-10T11:00:00Z".into());
    assert!(input.validate(&options).is_err());
}
