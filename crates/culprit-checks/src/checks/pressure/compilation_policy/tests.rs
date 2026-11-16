use super::*;
use crate::views::compilation::{policy::tests::measured_recompile, tests::native};
use culprit_core::{Check, QueryCosts, SourceOutcome, SourceState, telemetry::*};

fn recorded() -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    snapshot.workload = None;
    let mut database = snapshot.database_identity();
    database.incarnation = Some("synthetic-query-store-namespace".into());
    snapshot.identity = Some(database.clone());
    let mut row = measured_recompile();
    row.source = source_id::QUERY_COMPILATION.into();
    snapshot.operations.query_compilation = Some(CompilationSnapshot {
        context: Some(NativeReadContext {
            target: database.into(),
            partition: SourcePartition::Target,
            observed_from: "2026-11-15T12:00:00Z".into(),
            observed_until: "2026-11-15T12:00:01Z".into(),
            database_writable: Some(true),
        }),
        source: row.source.clone(),
        observed_from: "2026-11-15T12:00:00Z".into(),
        observed_until: "2026-11-15T12:00:01Z".into(),
        observations: vec![row],
    });
    snapshot.sources.push(SourceOutcome {
        source: source_id::QUERY_COMPILATION.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    snapshot
}

#[test]
fn recorded_policy_needs_no_runtime_denominator_and_cannot_borrow_live_waits_or_costs() {
    let snapshot = recorded();
    let finding = CompilationPolicyCheck.run(&snapshot).remove(0);
    assert_eq!(finding.actionability, Actionability::Information);
    assert!(finding.queries.is_empty());
    assert!(finding.metrics.iter().all(|metric| metric.unit != Unit::Ratio));
    assert!(finding.fix.unwrap().sql.is_none());
    assert_eq!(finding.facts.workload_dependency, Some(WorkloadDependency::Independent));
    assert!(finding.title.contains("OPTION (RECOMPILE)"));
    assert!(finding.caveats.contains(&t!("view.compilation.limit.lifetime")));
    assert!(finding.caveats.contains(&t!("view.compilation.limit.admission")));
}

#[test]
fn comparable_material_compilation_keeps_its_supported_severity_and_impact_instead_of_a_historical_case() {
    let mut snapshot = recorded();
    let raw = snapshot.operations.query_compilation.as_mut().unwrap();
    native(&mut raw.observations[0]).average_duration_us = 10_000_000.0;
    let identity = native(&mut raw.observations[0]).identity.clone();
    let template = crate::waits::snapshot_with(Engine::Mssql).workload.unwrap();
    let mut workload = template;
    workload.window_start = Some("2020-01-01T06:00:00Z".into());
    workload.window_end = Some("2020-01-01T12:00:00Z".into());
    workload.window_hours = 6.0;
    let mut query = workload.queries.iter().find(|q| !crate::support::is_monitoring_query(q)).unwrap().clone();
    query.id = "42".into();
    query.native_identity = Some(identity);
    query.text = "SELECT id FROM dbo.orders OPTION (RECOMPILE)".into();
    query.executions = 100.0;
    query.total_time_ms = 1_000.0;
    query.costs = Some(QueryCosts { plans: Some(12.0), plan_ms: Some(120_000.0), ..Default::default() });
    workload.queries = vec![query];
    snapshot.workload = Some(workload);
    assert!(CompilationPolicyCheck.run(&snapshot).is_empty());
    let finding = crate::signals::PlanningTimeCheck.run(&snapshot).remove(0);
    assert_eq!(finding.severity, Severity::Medium);
    assert!(finding.metrics.iter().any(|metric| metric.unit == Unit::Ratio && metric.value > 0.9));
    assert!(finding.fix.unwrap().summary.contains("keep the hint if"));
    snapshot.workload.as_mut().unwrap().queries[0].costs.as_mut().unwrap().plan_ms = None;
    let historical = CompilationPolicyCheck.run(&snapshot).remove(0);
    assert_eq!(historical.queries[0].id, "42");
    assert_eq!(historical.subject.name, "42");
    snapshot.workload.as_mut().unwrap().queries[0]
        .native_identity
        .as_mut()
        .unwrap()
        .qualifiers
        .insert("context_settings_id".into(), "other-context".into());
    assert!(
        CompilationPolicyCheck.run(&snapshot)[0].queries.is_empty(),
        "another native incarnation cannot acquire the original SQL link"
    );
}

#[test]
fn incomplete_optional_policy_and_conflicting_native_rows_do_not_become_a_finding() {
    let mut snapshot = recorded();
    let raw = snapshot.operations.query_compilation.as_mut().unwrap();
    native(&mut raw.observations[0]).statement =
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    assert!(CompilationPolicyCheck.run(&snapshot).is_empty());
    let mut snapshot = recorded();
    let raw = snapshot.operations.query_compilation.as_mut().unwrap();
    raw.observations.push(raw.observations[0].clone());
    assert!(CompilationPolicyCheck.run(&snapshot).is_empty(), "duplicate native entry cannot choose a representative");
}

#[test]
fn unknown_or_foreign_database_namespace_cannot_lend_a_query_link_or_comparable_cost_suppression() {
    let mut snapshot = recorded();
    let identity = match &snapshot.operations.query_compilation.as_ref().unwrap().observations[0].fact {
        OperationalFact::MssqlQueryCompilation(row) => row.identity.clone(),
        _ => panic!(),
    };
    let mut workload = crate::waits::snapshot_with(Engine::Mssql).workload.unwrap();
    workload.queries.truncate(1);
    let query = &mut workload.queries[0];
    query.native_identity = Some(identity);
    query.costs = Some(QueryCosts { plans: Some(12.0), plan_ms: Some(120_000.0), ..Default::default() });
    snapshot.workload = Some(workload);
    snapshot.operations.query_compilation.as_mut().unwrap().context = None;
    let finding = CompilationPolicyCheck.run(&snapshot).remove(0);
    assert!(finding.queries.is_empty());
    let mut foreign = recorded();
    let context = foreign.operations.query_compilation.as_mut().unwrap().context.as_mut().unwrap();
    if let CollectionTarget::Database(database) = &mut context.target {
        database.incarnation = Some("foreign-native-database".into());
    }
    assert!(CompilationPolicyCheck.run(&foreign).is_empty());
}
