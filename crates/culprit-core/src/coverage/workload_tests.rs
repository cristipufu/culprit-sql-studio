use super::*;
use crate::{
    RuntimePopulation, RuntimePopulationCatalog, RuntimePopulationMembership, Snapshot, WorkloadKind, i18n::Message,
    telemetry::NativeIdentity,
};

fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap()
}

fn observed(snapshot: &mut Snapshot) {
    let workload = snapshot.workload.as_mut().unwrap();
    let identity = NativeIdentity::new("7");
    workload.population = Some(identity.clone());
    workload.population_catalog = Some(RuntimePopulationCatalog {
        source: workload.evidence_source().into(),
        membership: RuntimePopulationMembership::ObservedWindow {
            from: workload.window_start.clone().unwrap(),
            until: workload.window_end.clone().unwrap(),
        },
        partitioned: true,
        requires_selection: false,
        populations: vec![RuntimePopulation {
            identity,
            label: Message::from("7"),
            native_properties: Default::default(),
        }],
    });
}

#[test]
fn workload_semantics_choose_the_coverage_source_without_reading_the_display_name() {
    let mut snapshot = snapshot();
    let workload = snapshot.workload.as_mut().unwrap();
    workload.source = "unrelated private display source".into();
    for (kind, source) in [
        (Some(WorkloadKind::QueryStore), source_id::QUERY_STORE),
        (Some(WorkloadKind::PlanCache), source_id::PLAN_CACHE),
        (Some(WorkloadKind::QueryLog), source_id::QUERY_LOG),
        (Some(WorkloadKind::StatementStats), source_id::WORKLOAD),
        (Some(WorkloadKind::PerformanceSchema), source_id::WORKLOAD),
        (Some(WorkloadKind::SlowLog), source_id::WORKLOAD),
        (None, source_id::WORKLOAD),
    ] {
        workload.kind = kind;
        assert_eq!(workload.evidence_source(), source);
    }
}

#[test]
fn recorded_failure_and_incomplete_membership_withhold_absence_without_inventing_a_missing_source_failure() {
    let mut snapshot = snapshot();
    snapshot.sources.clear();
    assert!(snapshot.workload_absence_problem().is_none(), "a missing outcome keeps its existing policy");
    observed(&mut snapshot);
    assert!(snapshot.workload_absence_problem().is_some(), "explicit observed membership is not complete history");
    snapshot.sources.push(SourceOutcome {
        source: source_id::QUERY_STORE.into(),
        state: SourceState::Unavailable,
        reason: Some("original native permission refusal".into()),
        reason_message: None,
    });
    assert_eq!(snapshot.workload_absence_problem().as_deref(), Some("original native permission refusal"));
    snapshot.sources.clear();
    snapshot.workload.as_mut().unwrap().population_catalog.as_mut().unwrap().membership =
        RuntimePopulationMembership::NativeCatalog;
    assert!(snapshot.workload_absence_problem().is_none());
}

#[test]
fn independent_cache_workload_does_not_borrow_an_unrelated_observed_catalog_gap() {
    let mut snapshot = snapshot();
    observed(&mut snapshot);
    snapshot.runtime_populations = snapshot.workload.as_mut().unwrap().population_catalog.take();
    let workload = snapshot.workload.as_mut().unwrap();
    workload.population = None;
    workload.kind = Some(WorkloadKind::PlanCache);
    snapshot.sources = vec![SourceOutcome {
        source: source_id::QUERY_STORE.into(),
        state: SourceState::Partial,
        reason: Some("different source membership is incomplete".into()),
        reason_message: None,
    }];
    assert!(snapshot.workload_absence_problem().is_none());
}
