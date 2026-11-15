use super::*;
use crate::waits::snapshot_with;
use culprit_core::{
    QueryCosts, QueryVariantFamily,
    telemetry::{NativePlanIdentity, NativeQueryIdentity},
};

fn measured(engine: Engine, events: Option<f64>, text: &str) -> Snapshot {
    let mut snapshot = snapshot_with(engine);
    let workload = snapshot.workload.as_mut().unwrap();
    let mut query = workload.queries.iter().find(|query| !crate::support::is_monitoring_query(query)).unwrap().clone();
    query.text = text.into();
    query.executions = 100.0;
    query.total_time_ms = 2_000.0;
    query.mean_time_ms = 20.0;
    query.costs = Some(QueryCosts { plan_ms: Some(6_000.0), plans: events, ..Default::default() });
    workload.queries = vec![query];
    snapshot
}

fn diagnose(snapshot: &Snapshot) -> Finding {
    let mut findings = PlanningTimeCheck.run(snapshot);
    assert_eq!(findings.len(), 1);
    findings.remove(0)
}

#[test]
fn equal_more_or_fewer_compiles_than_runs_never_establish_reuse_or_authorize_a_database_change() {
    for events in [1.0, 10.0, 100.0, 300.0] {
        let snapshot = measured(Engine::Mssql, Some(events), "SELECT id FROM dbo.orders WHERE status = @status");
        let case = planning_times(&snapshot).remove(0);
        assert!(matches!(case.investigation, PlanningInvestigation::CompilationCauseUnknown));
        assert_eq!(case.events, Some(events));
        assert_eq!(case.average_ms, Some(6_000.0 / events));
        let finding = diagnose(&snapshot);
        assert_eq!(finding.queries[0].id, snapshot.workload.as_ref().unwrap().queries[0].id);
        assert!(!finding.summary.contains("new plan for every run"));
        assert!(!finding.summary.contains("plan reused"));
        let advice = finding.fix.as_ref().unwrap();
        assert!(advice.sql.is_none());
        assert!(advice.summary.contains("schema or statistics changes"), "{}", advice.summary);
        assert!(advice.summary.contains("parameter-sensitive variants"), "{}", advice.summary);
        assert!(finding.verify.iter().any(|step| step.contains("combined cost improves")));
    }
}

#[test]
fn only_an_observed_statement_recompile_directive_selects_the_conditional_hint_investigation() {
    let snapshot =
        measured(Engine::Mssql, Some(10.0), "SELECT id FROM dbo.orders WHERE status = @status OPTION (RECOMPILE)");
    let case = planning_times(&snapshot).remove(0);
    assert!(matches!(case.investigation, PlanningInvestigation::StatementRecompileDirective));
    let finding = diagnose(&snapshot);
    let advice = finding.fix.as_ref().unwrap();
    assert!(advice.sql.is_none());
    assert!(advice.summary.contains("keep the hint if"), "{}", advice.summary);
    assert!(finding.verify.iter().any(|step| step.contains("representative parameter values")));

    for text in [
        "SELECT id FROM dbo.orders WHERE status = @status /* OPTION (RECOMPILE) */",
        "SELECT id FROM dbo.orders WHERE note = N'OPTION (RECOMPILE)'",
    ] {
        let snapshot = measured(Engine::Mssql, Some(100.0), text);
        let case = planning_times(&snapshot).remove(0);
        assert!(matches!(case.investigation, PlanningInvestigation::CompilationCauseUnknown));
        assert!(diagnose(&snapshot).fix.unwrap().sql.is_none());
    }
}

#[test]
fn missing_zero_or_invalid_event_counts_do_not_become_a_synthetic_one_compile_average() {
    for events in [None, Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
        let snapshot = measured(Engine::Mssql, events, "SELECT id FROM dbo.orders");
        let case = planning_times(&snapshot).remove(0);
        assert_eq!(case.average_ms, None);
        let finding = diagnose(&snapshot);
        assert!(finding.metrics.iter().all(|metric| metric.unit != Unit::Ms));
        assert!(!finding.summary.contains("recorded compilations average"));
        assert!(finding.fix.unwrap().sql.is_none());
    }
}

#[test]
fn a_native_variant_family_does_not_turn_compilation_work_into_a_fragmented_cache_diagnosis() {
    let mut snapshot = measured(Engine::Mssql, Some(100.0), "SELECT id FROM dbo.orders WHERE status = @status");
    let query = &mut snapshot.workload.as_mut().unwrap().queries[0];
    let variant = NativeQueryIdentity { query_id: "21".into(), qualifiers: Default::default() };
    let parent = NativeQueryIdentity { query_id: "10".into(), qualifiers: Default::default() };
    query.variant_family = Some(QueryVariantFamily {
        variant: variant.clone(),
        parent: parent.clone(),
        dispatcher: NativePlanIdentity { query: parent, plan_id: "11".into(), qualifiers: Default::default() },
    });
    query.native_identity = Some(variant);
    let case = planning_times(&snapshot).remove(0);
    assert!(matches!(case.investigation, PlanningInvestigation::CompilationCauseUnknown));
    let finding = diagnose(&snapshot);
    assert!(finding.fix.unwrap().sql.is_none());
    assert!(!finding.summary.contains("fragment"));
}

#[test]
fn measured_postgres_planning_does_not_assert_missing_preparation_or_reuse() {
    let snapshot = measured(Engine::Postgres, Some(100.0), "SELECT id FROM orders WHERE status = $1");
    let case = planning_times(&snapshot).remove(0);
    assert!(matches!(case.investigation, PlanningInvestigation::PreparationAndPlanChoice));
    let finding = diagnose(&snapshot);
    assert!(!finding.summary.contains("new plan for every run"));
    assert!(finding.fix.as_ref().unwrap().summary.contains("generic or custom plans"));
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
}

#[test]
fn a_compile_count_without_measured_planning_work_does_not_create_a_compilation_finding() {
    let mut snapshot = measured(Engine::Mssql, Some(100.0), "SELECT id FROM dbo.orders");
    snapshot.workload.as_mut().unwrap().queries[0].costs.as_mut().unwrap().plan_ms = None;
    assert!(PlanningTimeCheck.run(&snapshot).is_empty());
}
