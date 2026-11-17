use super::*;

const COUNTED: &str = "SELECT COUNT_BIG(*) FROM @Rows r WHERE (r.id > 9264)";

fn counted(sql: &str) -> String {
    let filtered = scan(1, ROWS).replace("</TableScan>", r#"<Predicate><ScalarOperator ScalarString="[r].[id]&gt;(9264)"><Compare CompareOp="GT"><ScalarOperator><Identifier><ColumnReference Table="[@Rows]" Alias="[r]" Column="id" /></Identifier></ScalarOperator><ScalarOperator><Const ConstValue="(9264)" /></ScalarOperator></Compare></ScalarOperator></Predicate></TableScan>"#);
    document(&format!(
        r#"<StmtSimple StatementText="{sql}" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="2"><QueryPlan>
<RelOp NodeId="0" PhysicalOp="Stream Aggregate" LogicalOp="Aggregate" Parallel="0" EstimateRows="1" EstimatedTotalSubtreeCost="2">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1" /></RunTimeInformation>
<StreamAggregate><DefinedValues><DefinedValue><ColumnReference Column="Expr1000" /><ScalarOperator ScalarString="COUNT_BIG(*)"><Aggregate AggType="countstar" Distinct="false" /></ScalarOperator></DefinedValue></DefinedValues>{}</StreamAggregate>
</RelOp></QueryPlan></StmtSimple>"#,
        filtered
    ))
}

fn completed_counts() -> String {
    culprit_core::t!("insight.relational_input.completed_input_counts", estimated = "100", actual = "20K")
}

#[test]
fn a_native_count_aggregate_preserves_qualified_consumed_filtered_and_final_output_populations() {
    let read = crate::plan_xml::test_read_context("app");
    let plan = parse(COUNTED, &counted(COUNTED), Some(&read));
    let input = &plan.root.children[0];
    assert_eq!(input.facts().relational_input.unwrap().qualified_kind(), Kind::TableValuedParameter);
    assert_eq!(input.facts().access_rows.unwrap().actual_total, Some(20000.0));
    assert_eq!(input.act_rows, Some(10000.0));
    assert_eq!(plan.root.act_rows, Some(1.0));
    assert_eq!(plan.root.facts().serial_execution_count.as_deref(), Some("1"));
    let insights = culprit_core::insights::insights(&plan);
    let concern = insights.iter().find(|insight| insight.rule == "misestimate").unwrap();
    assert!(concern.detail.contains(&completed_counts()));
    assert!(concern.detail.contains(&culprit_core::t!("insight.relational_input.root_output", actual = "1")));
    assert!(concern.fix_sql.is_none());
    assert!(!concern.fix.contains("UPDATE STATISTICS"));
}

#[test]
fn reported_runtime_rows_and_a_completed_scan_do_not_supply_missing_serial_ancestor_proof() {
    let read = crate::plan_xml::test_read_context("app");
    for xml in [
        counted(COUNTED).replacen("Parallel=\"0\"", "Parallel=\"1\"", 1),
        counted(COUNTED).replacen("Parallel=\"0\"", "", 1),
        counted(COUNTED).replacen("Thread=\"0\"", "Thread=\"1\"", 1),
    ] {
        let plan = parse(COUNTED, &xml, Some(&read));
        assert!(plan.root.facts().serial_execution_count.is_none());
        assert_eq!(plan.root.act_executions, Some(1.0));
        let input = &plan.root.children[0];
        assert!(input.facts().scan_completion.as_ref().unwrap().is_single_completed_serial_scan());
        assert_eq!(input.facts().relational_input.unwrap().qualified_kind(), Kind::TableValuedParameter);
        let insights = culprit_core::insights::insights(&plan);
        let concern = insights.iter().find(|insight| insight.rule == "misestimate").unwrap();
        assert!(!concern.detail.contains(&completed_counts()));
        assert!(concern.detail.contains(&culprit_core::t!(
            "insight.relational_input.access_counts",
            estimated = "100",
            actual = "20K"
        )));
    }
}

#[test]
fn count_expressions_do_not_repair_unknown_function_scope_or_a_changed_native_statement() {
    let read = crate::plan_xml::test_read_context("app");
    for (requested, observed) in [
        (COUNTED, COUNTED.replace("COUNT_BIG(*)", "dbo.hidden(r.id)")),
        (COUNTED, COUNTED.replace("r.id", "r.other_id")),
        ("SELECT SUM(dbo.hidden(r.id)) FROM @Rows r", "SELECT SUM(dbo.hidden(r.id)) FROM @Rows r".into()),
        ("SELECT COUNT_BIG(*) FROM (SELECT * FROM @Rows) r", "SELECT COUNT_BIG(*) FROM (SELECT * FROM @Rows) r".into()),
    ] {
        let plan = parse(requested, &counted(&observed), Some(&read));
        assert_eq!(plan.root.children[0].facts().relational_input.unwrap().qualified_kind(), Kind::Unknown);
        assert!(
            culprit_core::insights::insights(&plan).iter().all(|insight| !insight.detail.contains(&completed_counts()))
        );
    }
}

#[test]
fn original_native_count_binding_survives_bounded_definition_redaction_and_refuses_module_substitution() {
    use culprit_core::telemetry::*;
    let xml = counted(COUNTED);
    let row = crate::cached_definitions::tests::module_row(COUNTED, &xml);
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let layout = crate::retained_definitions::Layout {
        maximum_bytes: 32 * 1024,
        maximum_statement_bytes: 4096,
        database_setting: Some(true),
    };
    let native = crate::retained_definitions::decode(&row, &layout, Some(&source)).unwrap();
    assert!(native.relational_input_proof.is_some());
    let plan = crate::plan_xml::parse_retained_definition(&native, &source).unwrap();
    assert!(culprit_core::insights::insights(&plan).iter().any(|insight| insight.detail.contains(&completed_counts())));
    let mut changed = native;
    changed.module_parameters.as_mut().unwrap().sql_handle.replace_range(2..4, "99");
    assert!(crate::plan_xml::parse_retained_definition(&changed, &source).is_err());
}
