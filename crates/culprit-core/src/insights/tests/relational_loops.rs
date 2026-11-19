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

fn wrap_outer(plan: &mut Plan, id: u32, kind: NodeKind) {
    let input = plan.root.children.remove(0);
    let mut wrapper = measured(id, "native wrapper", kind, input.est_rows, input.act_rows.unwrap(), 1.0);
    wrapper.facts_mut().serial_execution_count = Some("1".into());
    wrapper.children.push(input);
    plan.root.children.insert(0, wrapper);
}

#[test]
fn serial_cardinality_preserving_wrappers_retain_the_original_tvp_and_repeated_work_diagnosis() {
    let original = loop_insight(&tvp_loop()).unwrap();
    for kinds in [vec![NodeKind::Compute], vec![NodeKind::Sort], vec![NodeKind::Compute, NodeKind::Sort]] {
        let mut plan = tvp_loop();
        for (at, kind) in kinds.into_iter().enumerate() {
            wrap_outer(&mut plan, 3 + at as u32, kind);
        }
        let observed = loop_insight(&plan).expect("the same TVP feeds the same measured repeated work");
        assert_eq!(observed.nodes, original.nodes);
        assert_eq!(observed.title, original.title);
        assert_eq!(observed.detail, original.detail);
        assert_eq!(observed.fix, original.fix);
        assert_eq!(observed.verify, original.verify);
        assert!(observed.fix_sql.is_none());
    }
}

fn omit_runtime(node: &mut PlanNode) {
    node.act_rows = None;
    node.act_executions = None;
    node.facts_mut().serial_execution_count = None;
    node.runtime = None;
    node.facts_mut().parallel_operator = Some(false);
}

#[test]
fn a_deferred_scalar_uses_measured_endpoints_without_filling_its_unknown_runtime() {
    let original = loop_insight(&tvp_loop()).unwrap();
    for kinds in
        [vec![NodeKind::Compute], vec![NodeKind::Compute, NodeKind::Sort], vec![NodeKind::Sort, NodeKind::Compute]]
    {
        let mut plan = tvp_loop();
        for (at, kind) in kinds.into_iter().enumerate() {
            wrap_outer(&mut plan, 3 + at as u32, kind);
        }
        let mut node = &mut plan.root.children[0];
        while !node.children.is_empty() {
            if node.kind == NodeKind::Compute {
                omit_runtime(node);
            }
            node = &mut node.children[0];
        }
        let observed = loop_insight(&plan).expect("observed TVP output and inner invocations agree");
        assert_eq!(observed.nodes, original.nodes);
        assert_eq!(observed.detail, original.detail);
        assert_eq!(observed.fix, original.fix);
        for scalar in plan.nodes().into_iter().filter(|node| node.kind == NodeKind::Compute) {
            assert!(scalar.act_rows.is_none());
            assert!(scalar.act_executions.is_none());
            assert!(scalar.facts().serial_execution_count.is_none());
        }
    }
}

#[test]
fn deferred_scalar_endpoints_and_every_supplied_population_must_remain_qualified() {
    for case in [
        "scalar-rows",
        "scalar-executions",
        "scalar-serial",
        "scalar-parallel",
        "scalar-runtime",
        "scalar-time",
        "scalar-access",
        "scalar-typed-access",
        "scalar-completion",
        "scalar-planned-parallel",
        "scalar-unknown-parallel",
        "scalar-object",
        "scalar-index",
        "scalar-estimate",
        "scalar-goal",
        "scalar-predicate",
        "scalar-seek",
        "sort-without-runtime",
        "filter",
        "aggregate",
        "spool",
        "join",
        "leaf-rows",
        "leaf-executions",
        "leaf-serial",
        "leaf-parallel",
        "leaf-estimate",
        "leaf-goal",
        "leaf-completion",
        "leaf-access",
        "leaf-local",
        "leaf-unresolved",
        "scalar-input",
        "join-serial",
        "join-parallel",
        "inner-executions",
        "inner-serial",
        "inner-parallel",
        "inner-access",
        "efficient-inner",
        "healthy-input",
    ] {
        let mut plan = tvp_loop();
        wrap_outer(&mut plan, 3, NodeKind::Compute);
        omit_runtime(&mut plan.root.children[0]);
        let scalar = &mut plan.root.children[0];
        match case {
            "scalar-rows" => scalar.act_rows = Some(10_000.0),
            "scalar-executions" => scalar.act_executions = Some(1.0),
            "scalar-serial" => scalar.facts_mut().serial_execution_count = Some("1".into()),
            "scalar-parallel" => scalar.facts_mut().parallel_threads = Some(1),
            "scalar-runtime" => {
                scalar.runtime = Some(crate::actual::NodeRuntime { cpu_ms: Some(1.0), ..Default::default() })
            }
            "scalar-time" => scalar.act_time_ms = Some(1.0),
            "scalar-access" => scalar.rows_read = Some(10_000.0),
            "scalar-typed-access" => scalar.facts_mut().access_rows = PlanAccessRows::new(None, Some(10_000.0)),
            "scalar-completion" => {
                scalar.facts_mut().scan_completion = scalar.children[0].facts().scan_completion.clone()
            }
            "scalar-planned-parallel" => scalar.facts_mut().parallel_operator = Some(true),
            "scalar-unknown-parallel" => scalar.facts_mut().parallel_operator = None,
            "scalar-object" => scalar.object = Some("dbo.Items".into()),
            "scalar-index" => scalar.index = Some("IX_Items_id".into()),
            "scalar-estimate" => scalar.facts_mut().estimated_rows_unavailable = true,
            "scalar-goal" => scalar.facts_mut().estimated_rows_without_goal = Some(200.0),
            "scalar-predicate" => scalar.predicate = Some("r.id=1".into()),
            "scalar-seek" => scalar.seek_predicate = Some("r.id=1".into()),
            "sort-without-runtime" => scalar.kind = NodeKind::Sort,
            "filter" => scalar.kind = NodeKind::Other,
            "aggregate" => scalar.kind = NodeKind::Aggregate,
            "spool" => scalar.kind = NodeKind::Spool,
            "join" => scalar.kind = NodeKind::Join,
            "leaf-rows" => scalar.children[0].act_rows = None,
            "leaf-executions" => scalar.children[0].act_executions = Some(2.0),
            "leaf-serial" => scalar.children[0].facts_mut().serial_execution_count = None,
            "leaf-parallel" => scalar.children[0].facts_mut().parallel_threads = Some(1),
            "leaf-estimate" => scalar.children[0].facts_mut().estimated_rows_unavailable = true,
            "leaf-goal" => scalar.children[0].facts_mut().estimated_rows_without_goal = Some(200.0),
            "leaf-completion" => scalar.children[0].facts_mut().scan_completion = None,
            "leaf-access" => scalar.children[0].facts_mut().access_rows = None,
            "leaf-local" => {
                scalar.children[0].facts_mut().relational_input = Some(RelationalInput {
                    kind: RelationalInputKind::LocalTableVariable,
                    provenance: RelationalInputProvenance::NativeDeclaration,
                })
            }
            "leaf-unresolved" => {
                scalar.children[0].facts_mut().relational_input = Some(RelationalInput {
                    kind: RelationalInputKind::Unknown,
                    provenance: RelationalInputProvenance::Unresolved,
                })
            }
            "scalar-input" => scalar.children[0].facts_mut().relational_input = None,
            "join-serial" => plan.root.facts_mut().serial_execution_count = None,
            "join-parallel" => plan.root.facts_mut().parallel_threads = Some(1),
            "inner-executions" => {
                plan.root.children[1].act_executions = Some(9_999.0);
                plan.root.children[1].facts_mut().serial_execution_count = Some("9999".into());
            }
            "inner-serial" => plan.root.children[1].facts_mut().serial_execution_count = None,
            "inner-parallel" => plan.root.children[1].facts_mut().parallel_threads = Some(1),
            "inner-access" => plan.root.children[1].facts_mut().access_rows = None,
            "efficient-inner" => {
                plan.root.children[1].facts_mut().access_rows = PlanAccessRows::new(None, Some(10_000.0))
            }
            "healthy-input" => {
                scalar.est_rows = 10_000.0;
                scalar.children[0].est_rows = 10_000.0;
                scalar.children[0].facts_mut().access_rows = PlanAccessRows::new(Some(20_000.0), Some(20_000.0));
            }
            _ => unreachable!(),
        }
        assert_no_loop(&plan, case);
    }
}

#[test]
fn observed_wrappers_require_one_native_serial_population_and_unchanged_rows() {
    for case in [
        "unknown-executions",
        "repeated",
        "parallel",
        "unknown-estimate",
        "changed-estimate",
        "changed-output",
        "unknown-output",
        "row-goal",
        "child-row-goal",
        "child-serial",
        "child-estimate",
        "duplicate-node",
        "extra-child",
        "filter",
        "aggregate",
        "spool",
        "join",
    ] {
        let mut plan = tvp_loop();
        wrap_outer(&mut plan, 3, NodeKind::Compute);
        let wrapper = &mut plan.root.children[0];
        match case {
            "unknown-executions" => wrapper.facts_mut().serial_execution_count = None,
            "repeated" => {
                wrapper.facts_mut().serial_execution_count = Some("2".into());
                wrapper.act_executions = Some(2.0);
            }
            "parallel" => wrapper.facts_mut().parallel_threads = Some(1),
            "unknown-estimate" => wrapper.facts_mut().estimated_rows_unavailable = true,
            "changed-estimate" => wrapper.est_rows = 200.0,
            "changed-output" => wrapper.act_rows = Some(9999.0),
            "unknown-output" => wrapper.act_rows = None,
            "row-goal" => wrapper.facts_mut().estimated_rows_without_goal = Some(200.0),
            "child-row-goal" => wrapper.children[0].facts_mut().estimated_rows_without_goal = Some(200.0),
            "child-serial" => wrapper.children[0].facts_mut().serial_execution_count = None,
            "child-estimate" => wrapper.children[0].facts_mut().estimated_rows_unavailable = true,
            "duplicate-node" => wrapper.id = wrapper.children[0].id,
            "extra-child" => wrapper.children.push(wrapper.children[0].clone()),
            "filter" => wrapper.kind = NodeKind::Other,
            "aggregate" => wrapper.kind = NodeKind::Aggregate,
            "spool" => wrapper.kind = NodeKind::Spool,
            "join" => wrapper.kind = NodeKind::Join,
            _ => unreachable!(),
        }
        assert_no_loop(&plan, case);
    }
}

#[test]
fn wrapped_healthy_or_unqualified_inputs_and_efficient_inner_access_do_not_become_tvp_faults() {
    for case in ["healthy", "scalar", "local", "unresolved", "efficient", "missing-inner-work", "unfinished"] {
        let mut plan = tvp_loop();
        match case {
            "healthy" => {
                plan.root.children[0].est_rows = 10_000.0;
                plan.root.children[0].facts_mut().access_rows = PlanAccessRows::new(Some(20_000.0), Some(20_000.0));
            }
            "scalar" => plan.root.children[0].facts_mut().relational_input = None,
            "local" => {
                plan.root.children[0].facts_mut().relational_input = Some(RelationalInput {
                    kind: RelationalInputKind::LocalTableVariable,
                    provenance: RelationalInputProvenance::NativeDeclaration,
                })
            }
            "unresolved" => {
                plan.root.children[0].facts_mut().relational_input = Some(RelationalInput {
                    kind: RelationalInputKind::Unknown,
                    provenance: RelationalInputProvenance::Unresolved,
                })
            }
            "efficient" => plan.root.children[1].facts_mut().access_rows = PlanAccessRows::new(None, Some(10_000.0)),
            "missing-inner-work" => plan.root.children[1].facts_mut().access_rows = None,
            "unfinished" => {
                plan.root.children[0].facts_mut().scan_completion.as_mut().unwrap().threads[0].end_of_scans =
                    Some("0".into())
            }
            _ => unreachable!(),
        }
        wrap_outer(&mut plan, 3, NodeKind::Compute);
        assert_no_loop(&plan, case);
    }
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
