use super::*;
use culprit_core::{
    CheckEvidence, RuntimePopulation, RuntimePopulationCatalog, RuntimePopulationMembership, SourceOutcome,
    SourceState, WorkloadKind, i18n::Message, telemetry::NativeIdentity,
};

fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    let workload = snapshot.workload.as_mut().unwrap();
    workload.kind = Some(WorkloadKind::QueryStore);
    workload.window_start = Some("2026-10-08T00:00:00Z".into());
    workload.window_end = Some("2026-10-11T12:00:00Z".into());
    workload.window_hours = 84.0;
    workload.timeline = None;
    workload.queries.truncate(1);
    let query = &mut workload.queries[0];
    query.executions = 1000.0;
    query.first_seen = workload.window_start.clone();
    query.last_seen = Some("2026-10-09T08:00:00Z".into());
    query.trend = None;
    let identity = NativeIdentity::new("7");
    workload.population = Some(identity.clone());
    workload.population_catalog = Some(RuntimePopulationCatalog {
        source: culprit_core::source_id::QUERY_STORE.into(),
        membership: RuntimePopulationMembership::NativeCatalog,
        partitioned: true,
        requires_selection: false,
        populations: vec![RuntimePopulation {
            identity,
            label: Message::from("7"),
            native_properties: Default::default(),
        }],
    });
    snapshot.sources = vec![SourceOutcome {
        source: culprit_core::source_id::QUERY_STORE.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    }];
    snapshot
}

#[test]
fn observed_window_keeps_positive_runtime_but_cannot_mark_a_statement_quiet_or_stopped() {
    let native = snapshot();
    let query = &native.workload.as_ref().unwrap().queries[0];
    assert!(query_quiet(&native, query).is_some());
    assert!(query_stopped(&native, query).is_some(), "native-catalog control has the same 52-hour silence");
    let finding = || {
        let mut finding = support::finding(
            "heavy-query",
            &query.id,
            culprit_core::Category::Queries,
            support::subject(SubjectKind::Query, &query.id),
        );
        finding.queries = vec![culprit_core::QueryRef { id: query.id.clone(), plan_node: None, note: None }];
        finding
    };
    let mut native_findings = vec![finding()];
    stopped_running(&native, &mut native_findings);
    assert!(native_findings[0].inactive);

    let mut observed = native.clone();
    let workload = observed.workload.as_mut().unwrap();
    workload.population_catalog.as_mut().unwrap().membership = RuntimePopulationMembership::ObservedWindow {
        from: workload.window_start.clone().unwrap(),
        until: workload.window_end.clone().unwrap(),
    };
    let query = &observed.workload.as_ref().unwrap().queries[0];
    assert_eq!(query.executions, 1000.0);
    assert_eq!(query.total_time_ms, native.workload.as_ref().unwrap().queries[0].total_time_ms);
    assert!(query_quiet(&observed, query).is_none());
    assert!(query_stopped(&observed, query).is_none());
    let mut observed_findings = vec![finding()];
    stopped_running(&observed, &mut observed_findings);
    assert!(!observed_findings[0].inactive);
    assert!(observed_findings[0].caveats.is_empty(), "no unsupported silence wording survives");
    let evidence =
        crate::evidence::assess(&[crate::evidence::Requirement::Query(crate::evidence::QueryMeasure::Work)], &observed);
    assert!(matches!(evidence, CheckEvidence::Insufficient(_)));
}

#[test]
fn recorded_partial_source_also_withholds_silence_without_erasing_the_native_last_seen_point() {
    let mut snapshot = snapshot();
    let query = &snapshot.workload.as_ref().unwrap().queries[0];
    assert!(query_stopped(&snapshot, query).is_some());
    snapshot.sources[0].state = SourceState::Partial;
    snapshot.sources[0].reason = Some("native history read was incomplete".into());
    let query = &snapshot.workload.as_ref().unwrap().queries[0];
    assert_eq!(query.last_seen.as_deref(), Some("2026-10-09T08:00:00Z"));
    assert!(query_quiet(&snapshot, query).is_none());
}
