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

fn access_plan(estimated_output: f64, actual_output: f64, estimated_access: f64, actual_access: f64) -> Plan {
    let input = RelationalInput { kind: Kind::TableValuedParameter, provenance: Provenance::NativeParameterMetadata };
    let mut plan = input_plan(Some(input), estimated_output, actual_output);
    plan.root.facts_mut().access_rows =
        crate::plan_facts::PlanAccessRows::new(Some(estimated_access), Some(actual_access));
    plan
}

#[test]
fn tvp_selectivity_discrepancy_is_distinct_from_access_volume_or_a_cached_input_size_claim() {
    let plan = access_plan(100.0, 10_000.0, 10_000.0, 10_000.0);
    let concern = estimate_concern(&plan);
    assert!(concern.detail.contains(&t!("insight.relational_input.selectivity")));
    assert!(concern.detail.contains(&t!("insight.relational_input.access_counts", estimated = "10K", actual = "10K")));
    assert!(concern.fix.starts_with(&t!("insight.relational_input.investigate_selectivity")));
    assert!(concern.fix.contains(&t!("insight.misestimate.fix.tvp", table = "@input")));
    assert_eq!(concern.verify, t!("insight.relational_input.verify"));
    assert!(concern.fix_sql.is_none());

    let volume = estimate_concern(&access_plan(100.0, 10_000.0, 100.0, 10_000.0));
    assert!(volume.detail.contains(&t!("insight.relational_input.access_volume")));
    assert!(volume.fix.starts_with(&t!("insight.relational_input.investigate_volume")));
    let both = estimate_concern(&access_plan(1.0, 10_000.0, 100.0, 10_000.0));
    assert!(both.detail.contains(&t!("insight.relational_input.access_and_selectivity")));
}

#[test]
fn convenience_rows_read_missing_actuals_partial_workers_or_invalid_counts_do_not_gain_access_provenance() {
    for missing in [
        "legacy-only",
        "estimated-only",
        "actual-only",
        "no-executions",
        "zero-executions",
        "fractional-executions",
        "parallel",
        "invalid",
        "inconsistent-output",
    ] {
        let mut plan = access_plan(100.0, 10_000.0, 10_000.0, 10_000.0);
        let node = &mut plan.root;
        match missing {
            "legacy-only" => {
                node.facts_mut().access_rows = None;
                node.rows_read = Some(10_000.0);
            }
            "estimated-only" => node.facts_mut().access_rows.as_mut().unwrap().actual_total = None,
            "actual-only" => node.facts_mut().access_rows.as_mut().unwrap().estimated_per_execution = None,
            "no-executions" => node.act_executions = None,
            "zero-executions" => node.act_executions = Some(0.0),
            "fractional-executions" => node.act_executions = Some(0.5),
            "parallel" => node.facts_mut().parallel_threads = Some(2),
            "invalid" => node.facts_mut().access_rows.as_mut().unwrap().actual_total = Some(f64::INFINITY),
            "inconsistent-output" => node.facts_mut().access_rows.as_mut().unwrap().actual_total = Some(100.0),
            _ => unreachable!(),
        }
        assert!(super::super::relational_inputs::AccessComparison::from_node(node).is_none(), "{missing}");
        if missing != "zero-executions" {
            let concern = estimate_concern(&plan);
            assert!(!concern.detail.contains(&t!("insight.relational_input.selectivity")), "{missing}");
        }
    }
}

#[test]
fn repeated_serial_access_totals_and_aggregated_final_output_keep_their_original_scopes() {
    let mut input = access_plan(1.0, 10_000.0, 10_000.0, 1_000_000.0).root;
    input.act_executions = Some(100.0);
    let mut aggregate = measured(0, "Stream Aggregate", NodeKind::Aggregate, 1.0, 1.0, 1.0);
    aggregate.children = vec![input];
    let concern = estimate_concern(&actual(Engine::Mssql, aggregate, 400.0));
    assert_eq!(concern.nodes, vec![1]);
    let million = format::count(1_000_000.0);
    assert!(concern.detail.contains(&t!(
        "insight.relational_input.access_counts",
        estimated = &million,
        actual = &million
    )));
    assert!(concern.detail.contains(&t!("insight.relational_input.selectivity")));
    assert!(concern.detail.contains("statement's final result"));
}

#[test]
fn healthy_large_filtered_or_unmeasured_tvp_inputs_do_not_create_a_cardinality_diagnosis() {
    let healthy = access_plan(1_000_000.0, 1_000_000.0, 1_000_000.0, 1_000_000.0);
    let filtered = access_plan(100.0, 100.0, 10_000.0, 10_000.0);
    let mut estimated = access_plan(100.0, 10_000.0, 10_000.0, 10_000.0);
    estimated.kind = PlanKind::Estimated;
    estimated.root.act_rows = None;
    estimated.root.act_executions = None;
    for plan in [healthy, filtered, estimated] {
        assert!(
            !insights(&plan).iter().any(|insight| matches!(insight.rule.as_str(), "misestimate" | "table-variable"))
        );
    }
    let mut limited = access_plan(10_000.0, 10.0, 10_000.0, 10.0);
    let mut top = measured(0, "Top", NodeKind::Other, 10.0, 10.0, 1.0);
    top.children = vec![limited.root];
    limited.root = top;
    assert!(!insights(&limited).iter().any(|insight| insight.rule == "misestimate"));
}

#[test]
fn relational_input_filter_and_spool_evidence_never_produce_direct_table_ddl() {
    for input in [
        RelationalInput { kind: Kind::TableValuedParameter, provenance: Provenance::NativeParameterMetadata },
        RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration },
        RelationalInput { kind: Kind::TemporaryTable, provenance: Provenance::NativePlan },
        RelationalInput { kind: Kind::TableValuedFunction, provenance: Provenance::NativePlan },
        RelationalInput { kind: Kind::Unknown, provenance: Provenance::Unresolved },
    ] {
        let mut plan = input_plan(Some(input), 100.0, 100.0);
        plan.root.rows_read = Some(100_000.0);
        plan.root.predicate = Some("[value]=(42)".into());
        let filtered =
            insights(&plan).into_iter().find(|insight| insight.rule == "scan-filter").expect("observed selective scan");
        assert!(filtered.fix_sql.is_none());
        assert_eq!(filtered.fix, statistics::relational_input_advice(input.kind, "@input"));
        let mut spool = measured(0, "Index Spool", NodeKind::Spool, 100.0, 100.0, 1.0);
        spool.detail = Some("Eager Spool".into());
        spool.seek_predicate = Some("[value]=(42)".into());
        spool.children = vec![plan.root];
        let concern = insights(&actual(Engine::Mssql, spool, 400.0))
            .into_iter()
            .find(|insight| insight.rule == "eager-spool")
            .unwrap();
        assert!(concern.fix_sql.is_none());
        assert_eq!(concern.fix, statistics::relational_input_advice(input.kind, "@input"));
    }
}

fn access_concern(plan: &Plan) -> PlanInsight {
    insights(plan)
        .into_iter()
        .find(|insight| insight.rule == "relational-input-access")
        .expect("recorded underestimated access")
}

#[test]
fn accurate_output_does_not_hide_offsetting_access_and_selectivity_errors() {
    let plan = access_plan(100.0, 100.0, 100.0, 10_000.0);
    assert!(plan.root.misestimates().is_empty(), "output matches its estimate");
    let concern = access_concern(&plan);
    assert_eq!(concern.nodes, vec![1]);
    assert_eq!(
        concern.title,
        t!("insight.relational_input_access.title", input = "@input", estimated = "100", actual = "10K")
    );
    assert!(concern.detail.contains(&t!("insight.relational_input.access_and_selectivity")));
    assert!(concern.detail.contains(&t!(
        "insight.relational_input_access.output_counts",
        estimated = "100",
        actual = "100"
    )));
    assert!(concern.fix.contains(&t!("insight.misestimate.fix.tvp", table = "@input")));
    assert!(concern.fix_sql.is_none());
    assert_eq!(concern.verify, t!("insight.relational_input.verify"));
    assert!(!insights(&plan).iter().any(|insight| insight.rule == "misestimate"));
}

#[test]
fn inner_loop_access_is_compared_across_its_actual_executions_not_its_outer_row_count() {
    let mut input = access_plan(1.0, 100.0, 100.0, 1_000_000.0).root;
    input.act_executions = Some(100.0);
    let mut join = measured(0, "Nested Loops", NodeKind::Join, 100.0, 100.0, 1.0);
    let mut outer = measured(2, "Table Scan", NodeKind::Scan, 100.0, 100.0, 1.0);
    outer.object = Some("dbo.drivers".into());
    join.children = vec![outer, input];
    let mut plan = actual(Engine::Mssql, join, 400.0);
    let concern = access_concern(&plan);
    assert_eq!(concern.nodes, vec![1]);
    assert!(concern.detail.contains(&t!(
        "insight.relational_input.access_counts",
        estimated = "10K",
        actual = format::count(1_000_000.0)
    )));
    assert!(concern.detail.contains(&t!(
        "insight.relational_input_access.output_counts",
        estimated = "100",
        actual = "100"
    )));
    plan.root.children[1].facts_mut().access_rows.as_mut().unwrap().actual_total = Some(10_000.0);
    assert!(!insights(&plan).iter().any(|insight| insight.rule == "relational-input-access"));
}

#[test]
fn unknown_or_other_native_input_kinds_never_borrow_tvp_advice() {
    for kind in [
        Some(RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration }),
        Some(RelationalInput { kind: Kind::TemporaryTable, provenance: Provenance::NativePlan }),
        Some(RelationalInput { kind: Kind::TableValuedFunction, provenance: Provenance::NativePlan }),
        Some(RelationalInput { kind: Kind::Unknown, provenance: Provenance::Unresolved }),
        None,
    ] {
        let mut plan = access_plan(100.0, 100.0, 100.0, 10_000.0);
        plan.root.facts_mut().relational_input = kind;
        let concern = access_concern(&plan);
        let expected_kind = kind.map_or(Kind::Unknown, |input| input.kind);
        assert!(concern.fix.ends_with(&statistics::relational_input_advice(expected_kind, "@input")));
        assert!(!concern.fix.contains(&t!("insight.misestimate.fix.tvp", table = "@input")));
        assert!(concern.fix_sql.is_none());
    }
    let mut ordinary_table = access_plan(100.0, 100.0, 100.0, 10_000.0);
    ordinary_table.root.object = Some("dbo.orders".into());
    ordinary_table.root.facts_mut().relational_input = None;
    assert!(!insights(&ordinary_table).iter().any(|insight| insight.rule == "relational-input-access"));
}

#[test]
fn estimated_missing_partial_worker_or_non_access_evidence_cannot_authorize_the_new_comparison() {
    for missing in [
        "estimated-plan",
        "no-native-access",
        "estimated-only",
        "actual-only",
        "no-executions",
        "fractional-executions",
        "parallel",
        "wrong-operator",
        "inconsistent-output",
    ] {
        let mut plan = access_plan(100.0, 100.0, 100.0, 10_000.0);
        match missing {
            "estimated-plan" => plan.kind = PlanKind::Estimated,
            "no-native-access" => {
                plan.root.facts_mut().access_rows = None;
                plan.root.rows_read = Some(10_000.0);
            }
            "estimated-only" => plan.root.facts_mut().access_rows.as_mut().unwrap().actual_total = None,
            "actual-only" => plan.root.facts_mut().access_rows.as_mut().unwrap().estimated_per_execution = None,
            "no-executions" => plan.root.act_executions = None,
            "fractional-executions" => plan.root.act_executions = Some(0.5),
            "parallel" => plan.root.facts_mut().parallel_threads = Some(2),
            "wrong-operator" => plan.root.kind = NodeKind::Join,
            "inconsistent-output" => plan.root.act_rows = Some(20_000.0),
            _ => unreachable!(),
        }
        assert!(!insights(&plan).iter().any(|insight| insight.rule == "relational-input-access"), "{missing}");
    }
}

#[test]
fn healthy_access_and_fewer_accesses_under_top_do_not_become_underestimation_findings() {
    for (estimated, actual) in [(10_000.0, 10_000.0), (10_000.0, 50_000.0), (10.0, 99.0)] {
        let plan = access_plan(1.0, 1.0, estimated, actual);
        assert!(!insights(&plan).iter().any(|insight| insight.rule == "relational-input-access"));
    }
    let mut top = measured(0, "Top", NodeKind::Other, 1.0, 1.0, 1.0);
    top.children = vec![access_plan(100.0, 1.0, 10_000.0, 1.0).root];
    let plan = actual(Engine::Mssql, top, 400.0);
    assert!(!insights(&plan).iter().any(|insight| insight.rule == "relational-input-access"));
}

#[test]
fn only_the_output_insight_that_already_covers_this_native_access_suppresses_it() {
    let plan = access_plan(100.0, 10_000.0, 100.0, 10_000.0);
    let output = estimate_concern(&plan);
    assert!(output.detail.contains(&t!("insight.relational_input.access_volume")));
    assert!(!insights(&plan).iter().any(|insight| insight.rule == "relational-input-access"));

    let mut independent = measured(2, "Table Scan", NodeKind::Scan, 1.0, 10_000.0, 1.0);
    independent.object = Some("dbo.orders".into());
    let mut root = measured(0, "Concatenation", NodeKind::Other, 1.0, 1.0, 1.0);
    root.children = vec![access_plan(100.0, 100.0, 100.0, 10_000.0).root, independent];
    let plan = actual(Engine::Mssql, root, 400.0);
    assert_eq!(estimate_concern(&plan).nodes, vec![2]);
    assert_eq!(access_concern(&plan).nodes, vec![1]);
}

#[test]
fn an_output_concern_omitted_by_its_bound_does_not_hide_an_unexplained_access_mismatch() {
    let mut root = measured(0, "Concatenation", NodeKind::Other, 1.0, 1.0, 1.0);
    root.children = [1_000_000.0, 100_000.0, 10_000.0]
        .into_iter()
        .enumerate()
        .map(|(index, actual)| {
            let mut input = access_plan(1.0, actual, 1.0, actual).root;
            input.id = u32::try_from(index + 1).unwrap();
            input.object = Some(format!("@input_{index}"));
            input
        })
        .collect();
    let plan = actual(Engine::Mssql, root, 400.0);
    let concerns = insights(&plan);
    let output: Vec<_> = concerns.iter().filter(|concern| concern.rule == "misestimate").collect();
    assert_eq!(output.len(), 2);
    assert!(output.iter().all(|concern| concern.nodes != vec![3]));
    assert_eq!(access_concern(&plan).nodes, vec![3]);
}
