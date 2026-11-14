use super::*;
use crate::plan_facts::{RelationalInput, RelationalInputKind as Kind, RelationalInputProvenance as Provenance};

fn input_plan(input: Option<RelationalInput>, estimate: f64, output: f64) -> Plan {
    let mut scan = measured(1, "Table Scan", NodeKind::Scan, estimate, output, 1.0);
    scan.object = Some("@input".into());
    scan.facts_mut().relational_input = input;
    actual(Engine::Mssql, scan, 400.0)
}

fn estimate_concern(plan: &Plan) -> PlanInsight {
    insights(plan).into_iter().find(|insight| insight.rule == "misestimate").expect("measured mismatch")
}

#[test]
fn native_kinds_share_the_recorded_comparison_without_a_one_row_story_or_duplicate_rule() {
    for input in [
        RelationalInput { kind: Kind::TableValuedParameter, provenance: Provenance::NativeParameterMetadata },
        RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration },
        RelationalInput { kind: Kind::Unknown, provenance: Provenance::Unresolved },
    ] {
        let plan = input_plan(Some(input), 100.0, 10_000.0);
        let concern = estimate_concern(&plan);
        assert_eq!(concern.title, "Expected 100 rows from @input, got 10K (100× more)");
        assert!(concern.fix_sql.is_none());
        assert!(!concern.detail.contains("single row"));
        assert!(!insights(&plan).iter().any(|insight| insight.rule == "table-variable"));
    }
}

#[test]
fn tvp_and_unresolved_input_do_not_receive_a_local_deferred_compilation_prescription() {
    let tvp = RelationalInput { kind: Kind::TableValuedParameter, provenance: Provenance::NativeParameterMetadata };
    let concern = estimate_concern(&input_plan(Some(tvp), 100.0, 10_000.0));
    assert!(concern.detail.contains("table-valued parameter"));
    assert!(concern.fix.contains("not a remedy"));
    assert!(!concern.fix.contains("150"));
    let local = RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration };
    let local = estimate_concern(&input_plan(Some(local), 100.0, 10_000.0));
    assert!(local.detail.contains("local table variable"));
    assert!(local.fix.contains("does not establish"));
    for input in [None, Some(RelationalInput { kind: Kind::TableValuedParameter, provenance: Provenance::NativePlan })]
    {
        let concern = estimate_concern(&input_plan(input, 100.0, 10_000.0));
        assert!(concern.detail.contains("native input kind is unverified"));
        assert!(!concern.fix.contains("150"));
        assert!(concern.fix_sql.is_none());
    }
}

#[test]
fn a_low_estimate_without_recorded_execution_is_not_a_proven_input_problem() {
    let input = RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration };
    let mut plan = input_plan(Some(input), 1.0, 10_000.0);
    plan.kind = PlanKind::Estimated;
    plan.root.act_rows = None;
    plan.root.act_executions = None;
    assert!(!insights(&plan).iter().any(|insight| matches!(insight.rule.as_str(), "misestimate" | "table-variable")));
}

#[test]
fn filtered_input_and_repeated_operator_executions_do_not_become_an_input_size_claim() {
    let input = RelationalInput { kind: Kind::TableValuedParameter, provenance: Provenance::NativeParameterMetadata };
    let mut filtered = input_plan(Some(input), 100.0, 100.0);
    filtered.root.rows_read = Some(10_000.0);
    assert!(!insights(&filtered).iter().any(|insight| insight.rule == "misestimate"));
    let mut repeated = input_plan(Some(input), 100.0, 10_000.0);
    repeated.root.act_executions = Some(100.0);
    assert!(!insights(&repeated).iter().any(|insight| insight.rule == "misestimate"));
    let input = input_plan(Some(input), 100.0, 10_000.0).root;
    let mut result = measured(0, "Stream Aggregate", NodeKind::Aggregate, 1.0, 1.0, 1.0);
    result.children = vec![input];
    let concern = estimate_concern(&actual(Engine::Mssql, result, 400.0));
    assert_eq!(concern.nodes, vec![1]);
    assert!(concern.detail.contains("not its unfiltered input size"));
    assert!(concern.detail.contains("statement's final result"));
}

#[test]
fn a_spill_above_an_input_mismatch_does_not_prove_the_mismatch_caused_it() {
    let input = RelationalInput { kind: Kind::TableValuedParameter, provenance: Provenance::NativeParameterMetadata };
    let mut parent = measured(0, "Hash Match", NodeKind::Join, 10_000.0, 10_000.0, 1.0);
    parent.warnings.push(PlanWarning::Spill);
    parent.children = vec![input_plan(Some(input), 100.0, 10_000.0).root];
    let concern = estimate_concern(&actual(Engine::Mssql, parent, 400.0));
    assert!(!concern.detail.contains("as a result"));
    assert!(concern.detail.contains("does not establish why"));
}

#[test]
fn native_temporary_and_function_inputs_never_receive_persistent_object_statistics_sql() {
    for input in [
        RelationalInput { kind: Kind::TemporaryTable, provenance: Provenance::NativePlan },
        RelationalInput { kind: Kind::TableValuedFunction, provenance: Provenance::NativePlan },
    ] {
        let mut plan = input_plan(Some(input), 100.0, 10_000.0);
        plan.root.object = Some("dbo.input_rows".into());
        assert!(estimate_concern(&plan).fix_sql.is_none());
    }
}

#[test]
fn a_native_function_is_not_a_dropped_table_when_the_call_stops_running() {
    let tables = [CatalogTable { schema: "dbo".into(), name: "orders".into(), indexes: vec![] }];
    let input = RelationalInput { kind: Kind::TableValuedFunction, provenance: Provenance::NativePlan };
    let mut plan = input_plan(Some(input), 100.0, 10_000.0);
    plan.root.object = Some("dbo.input_rows".into());
    mark_table_indexes(&mut plan.root, &tables, true);
    assert!(plan.root.facts().table_missing.is_none());
    let concern = estimate_concern(&plan);
    assert!(concern.fix.contains("native function kind"));
    plan.root.facts_mut().relational_input = None;
    mark_table_indexes(&mut plan.root, &tables, true);
    assert_eq!(plan.root.facts().table_missing.as_deref(), Some("dbo.input_rows"));
}
