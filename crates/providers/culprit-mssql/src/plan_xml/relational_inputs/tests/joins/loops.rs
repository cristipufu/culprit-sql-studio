use super::*;

fn loop_xml() -> String {
    let inner = scan(2, ITEMS)
        .replace(
            "PhysicalOp=\"Table Scan\" LogicalOp=\"Table Scan\"",
            "PhysicalOp=\"Index Seek\" LogicalOp=\"Index Seek\"",
        )
        .replace("<TableScan>", "<IndexScan>")
        .replace("</TableScan>", "</IndexScan>")
        .replace("ActualRowsRead=\"20000\"", "ActualRowsRead=\"2000000\"")
        .replace("ActualExecutions=\"1\"", "ActualExecutions=\"10000\"");
    join(SQL, &scan(1, ROWS), &inner, "")
        .replace("ActualRows=\"100\" ActualExecutions=\"1\"", "ActualRows=\"10000\" ActualExecutions=\"1\"")
}

fn concern(plan: &Plan) -> Option<culprit_core::insights::PlanInsight> {
    culprit_core::insights::insights(plan).into_iter().find(|insight| insight.rule == "nested-loops")
}

fn wrapped_loop_xml() -> String {
    let scan = scan(1, ROWS);
    let wrapper = format!(
        r#"<RelOp NodeId="3" PhysicalOp="Compute Scalar" LogicalOp="Compute Scalar" Parallel="0" EstimateRows="100" EstimatedTotalSubtreeCost="1">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualExecutions="1" /></RunTimeInformation>
<ComputeScalar>{scan}</ComputeScalar></RelOp>"#
    );
    loop_xml().replacen(&scan, &wrapper, 1)
}

#[test]
fn a_native_scalar_wrapper_preserves_exact_tvp_binding_and_measured_inner_work() {
    let read = crate::plan_xml::test_read_context("app");
    let plan = parse(SQL, &wrapped_loop_xml(), Some(&read));
    let wrapper = &plan.root.children[0];
    assert_eq!(wrapper.kind, culprit_core::NodeKind::Compute);
    assert_eq!(wrapper.facts().serial_execution_count.as_deref(), Some("1"));
    assert_eq!(wrapper.children[0].facts().relational_input.unwrap().qualified_kind(), Kind::TableValuedParameter);
    let observed = concern(&plan).expect("the native scalar does not erase the proven TVP loop");
    assert_eq!(observed.nodes, vec![0, 1, 2]);
    assert_eq!(observed.fix, culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows"));
    assert!(observed.fix_sql.is_none());
}

#[test]
fn native_deferred_compute_retains_measured_endpoints_without_projecting_scalar_runtime() {
    let read = crate::plan_xml::test_read_context("app");
    let xml = wrapped_loop_xml().replace(
        "<RunTimeInformation><RunTimeCountersPerThread Thread=\"0\" ActualRows=\"10000\" ActualExecutions=\"1\" /></RunTimeInformation>\n<ComputeScalar>",
        "<ComputeScalar>",
    );
    assert_ne!(xml, wrapped_loop_xml(), "the actual native omission must be represented");
    let plan = parse(SQL, &xml, Some(&read));
    let scalar = &plan.root.children[0];
    assert!(scalar.act_rows.is_none());
    assert!(scalar.act_executions.is_none());
    assert!(scalar.facts().serial_execution_count.is_none());
    assert_eq!(scalar.facts().parallel_operator, Some(false));
    let insight = concern(&plan).expect("the same qualified leaf and measured inner work remain actionable");
    assert_eq!(insight.nodes, vec![0, 1, 2]);
    assert_eq!(insight.fix, culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows"));

    for invalid in [
        xml.replace("NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"0\"", "NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"1\""),
        xml.replace("NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"0\"", "NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\""),
        xml.replace("NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"0\"", "NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"invalid\""),
        xml.replace("<ComputeScalar>", "<RunTimeInformation><RunTimeCountersPerThread Thread=\"0\" ActualRows=\"10000\" /></RunTimeInformation><ComputeScalar>"),
        xml.replace("<ComputeScalar>", "<RunTimeInformation><RunTimeCountersPerThread Thread=\"0\" ActualExecutions=\"1\" /></RunTimeInformation><ComputeScalar>"),
        xml.replace("ActualExecutions=\"10000\"", "ActualExecutions=\"9999\""),
        xml.replace("ActualRowsRead=\"2000000\"", "ActualRowsRead=\"10000\""),
        xml.replace("ActualEndOfScans=\"1\"", "ActualEndOfScans=\"0\""),
        xml.replace("NodeId=\"1\" PhysicalOp=\"Table Scan\" LogicalOp=\"Table Scan\" Parallel=\"0\"", "NodeId=\"1\" PhysicalOp=\"Table Scan\" LogicalOp=\"Table Scan\" Parallel=\"1\""),
        xml.replace("<ComputeScalar>", "<Filter><Predicate><ScalarOperator ScalarString=\"r.id=1\" /></Predicate>").replace("</ComputeScalar>", "</Filter>").replace("PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\"", "PhysicalOp=\"Filter\" LogicalOp=\"Filter\""),
        xml.replace("EstimateRows=\"100\"", "EstimateRows=\"10000\"").replace("EstimatedRowsRead=\"100\"", "EstimatedRowsRead=\"20000\""),
    ] {
        assert_ne!(invalid, xml, "each endpoint counterexample must change its evidence");
        assert!(concern(&parse(SQL, &invalid, Some(&read))).is_none());
    }
}

#[test]
fn native_wrapper_population_gaps_and_healthy_variants_never_supply_a_tvp_loop_fault() {
    let read = crate::plan_xml::test_read_context("app");
    let xml = wrapped_loop_xml();
    for invalid in [
        xml.replace("ActualRows=\"10000\" ActualExecutions=\"1\" /></RunTimeInformation>\n<ComputeScalar>", "ActualRows=\"9999\" ActualExecutions=\"1\" /></RunTimeInformation>\n<ComputeScalar>"),
        xml.replace("<ComputeScalar>", "<Filter>").replace("</ComputeScalar>", "</Filter>").replace("PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\"", "PhysicalOp=\"Filter\" LogicalOp=\"Filter\""),
        xml.replace("NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"0\" EstimateRows=\"100\"", "NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"0\""),
        xml.replace("NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"0\"", "NodeId=\"3\" PhysicalOp=\"Compute Scalar\" LogicalOp=\"Compute Scalar\" Parallel=\"1\""),
        xml.replace("ActualRowsRead=\"2000000\"", "ActualRowsRead=\"10000\""),
    ] {
        assert_ne!(invalid, xml, "each native counterexample must change the intended evidence");
        assert!(concern(&parse(SQL, &invalid, Some(&read))).is_none());
    }
    let healthy = xml
        .replace("<QueryPlan>", "<QueryPlan QueryVariantID=\"7\">")
        .replace("EstimateRows=\"100\"", "EstimateRows=\"10000\"")
        .replace("EstimatedRowsRead=\"100\"", "EstimatedRowsRead=\"20000\"");
    assert!(
        concern(&parse(SQL, &healthy, Some(&read))).is_none(),
        "a native variant with accurate TVP estimates is healthy"
    );
}

#[test]
fn the_same_wrapped_loop_requires_original_native_module_and_retained_binding_proof() {
    use culprit_core::telemetry::*;
    let xml = wrapped_loop_xml();
    for read in [None, Some(crate::plan_xml::test_read_context("other"))] {
        assert!(concern(&parse(SQL, &xml, read.as_ref())).is_none());
    }
    let row = crate::cached_definitions::tests::module_row(SQL, &xml);
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let layout = crate::retained_definitions::Layout {
        maximum_bytes: 32 * 1024,
        maximum_statement_bytes: 4096,
        database_setting: Some(true),
    };
    let original = crate::retained_definitions::decode(&row, &layout, Some(&source)).unwrap();
    assert!(concern(&crate::plan_xml::parse_retained_definition(&original, &source).unwrap()).is_some());
    let mut missing = original;
    missing.relational_input_proof = None;
    assert!(concern(&crate::plan_xml::parse_retained_definition(&missing, &source).unwrap()).is_none());
}

#[test]
fn one_selected_actual_plan_binds_native_tvp_output_to_measured_inner_access() {
    let read = crate::plan_xml::test_read_context("app");
    let plan = parse(SQL, &loop_xml(), Some(&read));
    assert_eq!(plan.kind, culprit_core::PlanKind::Actual);
    assert_eq!(plan.root.facts().inner_join, Some(true));
    assert_eq!(plan.root.facts().serial_execution_count.as_deref(), Some("1"));
    assert_eq!(plan.root.children[0].facts().serial_execution_count.as_deref(), Some("1"));
    assert_eq!(plan.root.children[1].facts().serial_execution_count.as_deref(), Some("10000"));
    assert_eq!(plan.root.children[1].facts().access_rows.unwrap().actual_total, Some(2_000_000.0));
    let observed = concern(&plan).expect("native repeated TVP work");
    assert_eq!(observed.nodes, vec![0, 1, 2]);
    assert!(observed.fix_sql.is_none());
    assert_eq!(observed.fix, culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows"));
    assert!(observed.detail.contains(&culprit_core::t!("insight.nested_loops.tvp.limit")));
}

#[test]
fn native_parallelism_missing_access_and_efficient_inner_seeks_do_not_fall_back_to_statistics_advice() {
    let read = crate::plan_xml::test_read_context("app");
    let original = loop_xml();
    for xml in [
        original.replace("Parallel=\"0\"", "Parallel=\"1\""),
        original.replace("Parallel=\"0\"", ""),
        original.replace("Thread=\"0\"", "Thread=\"1\""),
        original.replace("ActualRowsRead=\"2000000\"", ""),
        original.replace("ActualRowsRead=\"2000000\"", "ActualRowsRead=\"10000\""),
        original.replace("ActualExecutions=\"10000\"", "ActualExecutions=\"20000\""),
        original.replace("ActualEndOfScans=\"1\"", "ActualEndOfScans=\"0\""),
        original.replace("LogicalOp=\"Inner Join\"", "LogicalOp=\"Left Semi Join\""),
        original.replace("LogicalOp=\"Inner Join\"", "LogicalOp=\"Unrecognized Join\""),
    ] {
        let plan = parse(SQL, &xml, Some(&read));
        assert!(concern(&plan).is_none(), "{xml}");
    }
}

#[test]
fn current_or_missing_namespaces_and_changed_original_statement_cannot_supply_a_tvp_binding() {
    let xml = loop_xml();
    for read in [None, Some(crate::plan_xml::test_read_context("other"))] {
        let plan = parse(SQL, &xml, read.as_ref());
        assert_eq!(kinds(&plan), vec![Kind::Unknown]);
        assert!(concern(&plan).is_none());
    }
    let read = crate::plan_xml::test_read_context("app");
    let changed = xml.replace(SQL, &SQL.replace("r.id", "r.other_id"));
    let plan = parse(SQL, &changed, Some(&read));
    assert!(concern(&plan).is_none());
    assert_eq!(kinds(&plan), vec![Kind::Unknown]);
}

#[test]
fn sibling_runtime_or_a_matching_display_join_cannot_authorize_the_selected_statement() {
    let read = crate::plan_xml::test_read_context("app");
    let runtime = loop_xml();
    let estimated =
        runtime.lines().filter(|line| !line.contains("<RunTimeInformation>")).collect::<Vec<_>>().join("\n");
    let sibling = runtime
        .split_once("<Statements>")
        .unwrap()
        .1
        .split_once("</Statements>")
        .unwrap()
        .0
        .replace("0x0123456789ABCDEF", "0xFEDCBA9876543210");
    let selected = estimated.replace("</Statements>", &format!("{sibling}</Statements>"));
    let plan = parse(SQL, &selected, Some(&read));
    assert_eq!(plan.kind, culprit_core::PlanKind::Estimated);
    assert!(concern(&plan).is_none());
    assert!(plan.root.facts().serial_execution_count.is_none());
    assert_eq!(plan.root.facts().inner_join, Some(true));
}

#[test]
fn retained_native_module_proof_survives_redaction_without_borrowing_todays_module() {
    use culprit_core::telemetry::*;
    let xml = loop_xml();
    let row = crate::cached_definitions::tests::module_row(SQL, &xml);
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let layout = crate::retained_definitions::Layout {
        maximum_bytes: 32 * 1024,
        maximum_statement_bytes: 4096,
        database_setting: Some(true),
    };
    let native = crate::retained_definitions::decode(&row, &layout, Some(&source)).unwrap();
    assert!(native.relational_input_proof.is_some());
    let plan = crate::plan_xml::parse_retained_definition(&native, &source).unwrap();
    assert!(concern(&plan).is_some());
    let mut missing = native.clone();
    missing.relational_input_proof = None;
    let plan = crate::plan_xml::parse_retained_definition(&missing, &source).unwrap();
    assert!(concern(&plan).is_none());
    let mut changed = native;
    changed.module_parameters.as_mut().unwrap().sql_handle.replace_range(2..4, "99");
    assert!(crate::plan_xml::parse_retained_definition(&changed, &source).is_err());
}
