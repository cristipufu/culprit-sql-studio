use super::*;
use crate::plan_facts::{
    PlanAccessRows, PlanScanCompletion, PlanScanThread, RelationalInput, RelationalInputKind, RelationalInputProvenance,
};

fn tvp_loop() -> Plan {
    let mut outer = measured(1, "Table Scan", NodeKind::Scan, 100.0, 10_000.0, 1.0);
    outer.object = Some("@Rows".into());
    outer.facts_mut().relational_input = Some(RelationalInput {
        kind: RelationalInputKind::TableValuedParameter,
        provenance: RelationalInputProvenance::NativeParameterMetadata,
    });
    outer.facts_mut().access_rows = PlanAccessRows::new(Some(100.0), Some(20_000.0));
    outer.facts_mut().serial_execution_count = Some("1".into());
    outer.facts_mut().scan_completion = Some(PlanScanCompletion {
        parallel: Some(false),
        threads: vec![PlanScanThread {
            thread: 0,
            executions: Some("1".into()),
            end_of_scans: Some("1".into()),
            rebinds: None,
            rewinds: None,
        }],
    });
    let mut inner = measured(2, "Index Seek", NodeKind::Seek, 1.0, 10_000.0, 10_000.0);
    inner.object = Some("dbo.Items".into());
    inner.index = Some("IX_Items_id".into());
    inner.seek_predicate = Some("i.id = r.id".into());
    inner.facts_mut().access_rows = PlanAccessRows::new(None, Some(2_000_000.0));
    inner.facts_mut().serial_execution_count = Some("10000".into());
    let mut join = measured(0, "Nested Loops", NodeKind::Join, 100.0, 10_000.0, 1.0);
    join.facts_mut().serial_execution_count = Some("1".into());
    join.facts_mut().inner_join = Some(true);
    join.children = vec![outer, inner];
    let mut plan = actual(Engine::Mssql, join, 500.0);
    plan.statement = Some("SELECT r.id FROM @Rows r JOIN dbo.Items i ON r.id = i.id".into());
    plan
}

fn loop_insight(plan: &Plan) -> Option<PlanInsight> {
    insights(plan).into_iter().find(|insight| insight.rule == "nested-loops")
}

fn assert_no_loop(plan: &Plan, case: &str) {
    assert!(loop_insight(plan).is_none(), "{case}");
}

#[test]
fn actual_tvp_output_and_material_repeated_inner_access_are_reported_without_a_reuse_or_join_verdict() {
    let plan = tvp_loop();
    let observed = loop_insight(&plan).expect("same-run native TVP and inner work");
    assert_eq!(observed.nodes, vec![0, 1, 2]);
    assert!(observed.detail.contains(&t!(
        "insight.nested_loops.tvp.outer",
        input = "@Rows",
        estimated = rows_text(100.0),
        actual = rows_text(10_000.0)
    )));
    assert!(observed.detail.contains(&t!(
        "insight.nested_loops.tvp.inner",
        inner = "dbo.Items",
        executions = format::count(10_000.0),
        accessed = rows_text(2_000_000.0),
        per_execution = rows_text(200.0)
    )));
    assert!(observed.detail.contains(&t!("insight.nested_loops.tvp.limit")));
    assert_eq!(observed.fix, t!("insight.misestimate.fix.tvp", table = "@Rows"));
    assert!(observed.fix_sql.is_none());
    assert!(!observed.detail.contains(&t!("insight.nested_loops.detail.misjudged_hash")));
    assert!(!observed.fix.contains(&t!("insight.nested_loops.fix.estimate", outer = "@Rows")));
}

#[test]
fn an_inner_relational_input_cannot_bypass_qualified_tvp_analysis_through_generic_loop_advice() {
    let mut ordinary = tvp_loop();
    let tvp = ordinary.root.children[0].facts_mut().relational_input.take();
    ordinary.root.children[0].object = Some("dbo.Events".into());
    ordinary.statement = Some("SELECT e.id FROM dbo.Events e JOIN dbo.Items i ON e.id = i.id".into());
    let control = loop_insight(&ordinary).expect("the same measured ordinary-table loop reaches the generic rule");
    assert!(control.detail.contains(&t!("insight.nested_loops.detail.misjudged_hash")));
    assert_eq!(control.fix, t!("insight.nested_loops.fix.estimate", outer = "Events"));

    for classification in [
        tvp,
        Some(RelationalInput { kind: RelationalInputKind::Unknown, provenance: RelationalInputProvenance::Unresolved }),
        None,
    ] {
        let mut input = ordinary.clone();
        input.statement = Some("SELECT r.id FROM dbo.Events e JOIN @Rows r ON e.id = r.id".into());
        let inner = &mut input.root.children[1];
        inner.object = Some("@Rows".into());
        inner.index = Some("IX_Rows_id".into());
        inner.seek_predicate = Some("r.id = e.id".into());
        inner.facts_mut().relational_input = classification;
        assert_no_loop(&input, "an inner TVP or unresolved variable cannot inherit persistent-table advice");

        let inner = input.root.children.pop().unwrap();
        let mut wrapper = measured(3, "Compute Scalar", NodeKind::Compute, 1.0, 10_000.0, 10_000.0);
        wrapper.children.push(inner);
        input.root.children.push(wrapper);
        assert_no_loop(&input, "a relational input inside the inner subtree has the same evidence boundary");
    }
}

#[test]
fn efficient_seeks_and_healthy_large_tvp_estimates_are_not_loop_faults() {
    let mut efficient = tvp_loop();
    efficient.root.children[1].facts_mut().access_rows = PlanAccessRows::new(None, Some(10_000.0));
    assert_no_loop(&efficient, "one row accessed per invocation");
    let mut healthy = tvp_loop();
    healthy.root.children[0].est_rows = 10_000.0;
    healthy.root.children[0].facts_mut().access_rows = PlanAccessRows::new(Some(20_000.0), Some(20_000.0));
    assert_no_loop(&healthy, "large, accurately estimated input");
    healthy.facts_mut().parameter_context = Some(crate::plan_facts::PlanParameterContext {
        retrieved_from_cache: None,
        parameters: vec![crate::plan_facts::PlanParameter {
            integer_values_equal: None,
            name: Some("@filter".into()),
            data_type: Some("int".into()),
            compiled: crate::plan_facts::PlanParameterValueState::Recorded,
            runtime: crate::plan_facts::PlanParameterValueState::Recorded,
            comparison: crate::plan_facts::PlanParameterComparison::DifferentRepresentation,
        }],
        uses: vec![],
        issues: vec![],
    });
    assert_no_loop(&healthy, "a parameter representation difference cannot establish harmful reuse");
    assert!(insights(&healthy).iter().any(|insight| insight.rule == "parameter-context"));
}

#[test]
fn access_filtering_and_final_aggregation_keep_their_native_row_populations() {
    let mut plan = tvp_loop();
    let mut aggregate = measured(3, "Stream Aggregate", NodeKind::Aggregate, 1.0, 1.0, 1.0);
    aggregate.facts_mut().serial_execution_count = Some("1".into());
    aggregate.children.push(plan.root);
    plan = actual(Engine::Mssql, aggregate, 500.0);
    let observed = loop_insight(&plan).expect("aggregate output does not erase inner work");
    assert!(observed.detail.contains(&t!("insight.nested_loops.tvp.result", actual = "1")));
    assert!(observed.detail.contains(&t!("insight.relational_input.access_counts", estimated = "100", actual = "20K")));
    assert!(!observed.detail.contains(&t!(
        "insight.relational_input.completed_input_counts",
        estimated = "100",
        actual = "20K"
    )));
    let mut selectivity = tvp_loop();
    selectivity.root.children[0].est_rows = 10_000.0;
    assert_no_loop(&selectivity, "access underestimation alone is not outer output underestimation");
}

#[test]
fn missing_estimated_or_actual_read_counters_never_fall_back_to_output_or_legacy_rows_read() {
    for case in ["inner", "outer", "outer-estimate", "inner-output", "outer-output", "estimated"] {
        let mut plan = tvp_loop();
        match case {
            "inner" => {
                plan.root.children[1].facts_mut().access_rows = None;
                plan.root.children[1].rows_read = Some(2_000_000.0);
            }
            "outer" => plan.root.children[0].facts_mut().access_rows = PlanAccessRows::new(Some(100.0), None),
            "outer-estimate" => {
                plan.root.children[0].facts_mut().access_rows = PlanAccessRows::new(None, Some(20_000.0))
            }
            "inner-output" => plan.root.children[1].act_rows = None,
            "outer-output" => plan.root.children[0].act_rows = None,
            "estimated" => plan.kind = PlanKind::Estimated,
            _ => unreachable!(),
        }
        assert_no_loop(&plan, case);
    }
}

#[test]
fn absent_parallel_repeated_or_inexact_execution_scope_cannot_authorize_the_case() {
    for case in [
        "missing-join",
        "missing-outer",
        "missing-inner",
        "worker",
        "inexact",
        "noncanonical",
        "mismatch",
        "join-repeat",
        "outer-repeat",
        "outer-eof",
        "outer-rewind",
        "wrong-inner-count",
        "duplicate-id",
    ] {
        let mut plan = tvp_loop();
        match case {
            "missing-join" => plan.root.facts_mut().serial_execution_count = None,
            "missing-outer" => plan.root.children[0].facts_mut().serial_execution_count = None,
            "missing-inner" => plan.root.children[1].facts_mut().serial_execution_count = None,
            "worker" => plan.root.children[1].facts_mut().parallel_threads = Some(1),
            "inexact" => {
                plan.root.children[1].facts_mut().serial_execution_count = Some("9007199254740993".into());
                plan.root.children[1].act_executions = Some(9_007_199_254_740_992.0);
            }
            "noncanonical" => plan.root.children[1].facts_mut().serial_execution_count = Some("010000".into()),
            "mismatch" => plan.root.children[1].act_executions = Some(20_000.0),
            "join-repeat" => {
                plan.root.facts_mut().serial_execution_count = Some("2".into());
                plan.root.act_executions = Some(2.0);
            }
            "outer-repeat" => {
                plan.root.children[0].facts_mut().serial_execution_count = Some("2".into());
                plan.root.children[0].act_executions = Some(2.0);
            }
            "outer-eof" => {
                plan.root.children[0].facts_mut().scan_completion.as_mut().unwrap().threads[0].end_of_scans =
                    Some("0".into())
            }
            "outer-rewind" => {
                plan.root.children[0].facts_mut().scan_completion.as_mut().unwrap().threads[0].rewinds =
                    Some("1".into())
            }
            "wrong-inner-count" => {
                plan.root.children[1].facts_mut().serial_execution_count = Some("20000".into());
                plan.root.children[1].act_executions = Some(20_000.0);
            }
            "duplicate-id" => plan.root.children[1].id = 1,
            _ => unreachable!(),
        }
        assert_no_loop(&plan, case);
    }
}

#[test]
fn row_goals_semi_joins_spools_and_intermediate_input_operators_withhold_the_stronger_relationship() {
    for case in ["row-goal", "semi", "unknown-join", "top", "spool", "filtered-outer", "aggregate-outer"] {
        let mut plan = tvp_loop();
        match case {
            "row-goal" => plan.root.facts_mut().estimated_rows_without_goal = Some(1000.0),
            "semi" => plan.root.facts_mut().inner_join = Some(false),
            "unknown-join" => {
                plan.root.facts_mut().inner_join = None;
                plan.root.detail = Some("Inner Join".into());
            }
            "top" => {
                let mut top = measured(3, "Top", NodeKind::Other, 1.0, 1.0, 1.0);
                top.facts_mut().serial_execution_count = Some("1".into());
                top.children.push(plan.root);
                plan = actual(Engine::Mssql, top, 500.0);
            }
            "spool" => {
                let mut spool = measured(3, "Table Spool", NodeKind::Spool, 1.0, 10_000.0, 10_000.0);
                spool.children.push(plan.root.children.pop().unwrap());
                plan.root.children.push(spool);
            }
            "filtered-outer" | "aggregate-outer" => {
                let outer = plan.root.children.remove(0);
                let (op, kind) = if case == "filtered-outer" {
                    ("Filter", NodeKind::Other)
                } else {
                    ("Aggregate", NodeKind::Aggregate)
                };
                let mut wrapper = measured(3, op, kind, 100.0, 10_000.0, 1.0);
                wrapper.children.push(outer);
                plan.root.children.insert(0, wrapper);
            }
            _ => unreachable!(),
        }
        assert_no_loop(&plan, case);
    }
}

#[test]
fn a_variable_name_or_wrong_kind_provenance_never_receives_tvp_or_persistent_statistics_loop_advice() {
    for input in [
        None,
        Some(RelationalInput {
            kind: RelationalInputKind::TableValuedParameter,
            provenance: RelationalInputProvenance::NativePlan,
        }),
        Some(RelationalInput {
            kind: RelationalInputKind::LocalTableVariable,
            provenance: RelationalInputProvenance::NativeDeclaration,
        }),
        Some(RelationalInput {
            kind: RelationalInputKind::TemporaryTable,
            provenance: RelationalInputProvenance::NativePlan,
        }),
        Some(RelationalInput {
            kind: RelationalInputKind::TableValuedFunction,
            provenance: RelationalInputProvenance::NativePlan,
        }),
    ] {
        let mut plan = tvp_loop();
        plan.root.children[0].facts_mut().relational_input = input;
        assert_no_loop(&plan, "source-qualified TVP metadata is required");
    }
}
