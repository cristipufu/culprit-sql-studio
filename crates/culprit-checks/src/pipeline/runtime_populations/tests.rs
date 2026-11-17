use super::*;
use culprit_core::{
    RuntimePopulation, RuntimePopulationCatalog, RuntimePopulationMembership, i18n::Message, telemetry::NativeIdentity,
};

fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let workload = snapshot.workload.as_mut().unwrap();
    workload.population = Some(NativeIdentity::new("1"));
    workload.population_catalog = Some(RuntimePopulationCatalog {
        source: culprit_core::source_id::QUERY_STORE.into(),
        membership: RuntimePopulationMembership::ObservedWindow {
            from: workload.window_start.clone().unwrap(),
            until: workload.window_end.clone().unwrap(),
        },
        partitioned: true,
        requires_selection: false,
        populations: vec![RuntimePopulation {
            identity: NativeIdentity::new("1"),
            label: Message::from("observed"),
            native_properties: Default::default(),
        }],
    });
    snapshot.sources.retain(|source| source.source != culprit_core::source_id::QUERY_STORE);
    snapshot.sources.push(SourceOutcome {
        source: culprit_core::source_id::QUERY_STORE.into(),
        state: SourceState::Empty,
        reason: None,
        reason_message: None,
    });
    snapshot
}

#[test]
fn observed_membership_is_partial_without_erasing_positive_counts_or_independent_usage_evidence() {
    let mut snapshot = snapshot();
    let workload = serde_json::to_value(&snapshot.workload).unwrap();
    let others = snapshot
        .sources
        .iter()
        .filter(|source| source.source != culprit_core::source_id::QUERY_STORE)
        .cloned()
        .collect::<Vec<_>>();
    qualify(&mut snapshot);
    assert_eq!(snapshot.source_outcome(culprit_core::source_id::QUERY_STORE).unwrap().state, SourceState::Partial);
    assert_eq!(serde_json::to_value(&snapshot.workload).unwrap(), workload);
    assert_eq!(
        snapshot
            .sources
            .iter()
            .filter(|source| source.source != culprit_core::source_id::QUERY_STORE)
            .cloned()
            .collect::<Vec<_>>(),
        others
    );
}

#[test]
fn native_catalog_completeness_and_original_source_failure_are_preserved() {
    let mut native = snapshot();
    native.workload.as_mut().unwrap().population_catalog.as_mut().unwrap().membership =
        RuntimePopulationMembership::NativeCatalog;
    qualify(&mut native);
    assert_eq!(native.source_outcome(culprit_core::source_id::QUERY_STORE).unwrap().state, SourceState::Empty);
    let mut unavailable = snapshot();
    let source =
        unavailable.sources.iter_mut().find(|source| source.source == culprit_core::source_id::QUERY_STORE).unwrap();
    source.state = SourceState::Unavailable;
    source.reason = Some("original native permission refusal".into());
    qualify(&mut unavailable);
    let source = unavailable.source_outcome(culprit_core::source_id::QUERY_STORE).unwrap();
    assert_eq!(source.state, SourceState::Unavailable);
    assert_eq!(source.reason.as_deref(), Some("original native permission refusal"));
}
