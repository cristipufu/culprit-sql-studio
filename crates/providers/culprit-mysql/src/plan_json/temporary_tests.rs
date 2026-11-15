//! Native plan facts stay independent of display text, statement counters and storage-placement assumptions.
use super::*;
use culprit_core::plan_facts::PlanTemporaryTableUse;
use serde_json::json;

fn mysql_wrapper(wrapper: &str, temporary: Value, filesort: Value) -> Plan {
    let plan = json!({"query_block": {(wrapper): {
        "using_temporary_table": temporary,
        "using_filesort": filesort,
        "table": {"table_name":"orders", "access_type":"ALL", "rows_examined_per_scan":100000,
            "rows_produced_per_join":100000, "cost_info":{"read_cost":"50", "eval_cost":"50"}}
    }}});
    parse(&plan.to_string(), PlanKind::Estimated, None, Some("shop")).unwrap()
}

fn root_operation(plan: &Plan) -> &PlanNode {
    let [operation] = plan.root.children.as_slice() else { panic!("one native operation in this query block") };
    operation
}

#[test]
fn mysql_native_wrappers_identify_temporary_purpose_without_sql_or_filesort_inference() {
    for (wrapper, expected) in [
        ("grouping_operation", PlanTemporaryTableUse::Grouping),
        ("duplicates_removal", PlanTemporaryTableUse::DuplicateRemoval),
        ("ordering_operation", PlanTemporaryTableUse::Ordering),
        ("buffer_result", PlanTemporaryTableUse::Unknown),
    ] {
        let plan = mysql_wrapper(wrapper, json!(true), json!(false));
        assert_eq!(root_operation(&plan).facts().temporary_table, Some(expected));
        assert_eq!(root_operation(&plan).facts().filesort, Some(false));
        assert!(!plan.has_warning(PlanWarning::Spill));
        assert!(plan.statement.is_none());
    }
}

#[test]
fn false_missing_and_malformed_native_flags_do_not_become_positive_temporary_or_sort_evidence() {
    for (temporary, filesort, expected_sort) in [
        (json!(false), json!(true), Some(true)),
        (Value::Null, Value::Null, None),
        (json!("true"), json!("true"), None),
    ] {
        let plan = mysql_wrapper("grouping_operation", temporary, filesort);
        assert_eq!(root_operation(&plan).facts().temporary_table, None);
        assert_eq!(root_operation(&plan).facts().filesort, expected_sort);
        assert!(!plan.has_warning(PlanWarning::Spill));
    }
    let plan = parse(r#"{"query_block":{"grouping_operation":{"table":{"table_name":"orders","access_type":"ALL","rows_examined_per_scan":100000,"rows_produced_per_join":100000}}}}"#,
        PlanKind::Estimated, None, Some("shop")).unwrap();
    assert!(root_operation(&plan).facts().temporary_table.is_none());
    assert!(root_operation(&plan).facts().filesort.is_none());
}

#[test]
fn materialized_subquery_and_union_are_not_grouping_and_do_not_create_standalone_warnings() {
    let subquery = r#"{"query_block":{"table":{"table_name":"<derived2>","access_type":"ALL",
        "rows_examined_per_scan":100000,"rows_produced_per_join":100000,
        "materialized_from_subquery":{"using_temporary_table":true,"query_block":{"table":{
            "table_name":"orders","access_type":"ALL","rows_examined_per_scan":100000,"rows_produced_per_join":100000}}}}}}"#;
    let plan = parse(subquery, PlanKind::Estimated, None, Some("shop")).unwrap();
    assert_eq!(root_operation(&plan).facts().temporary_table, Some(PlanTemporaryTableUse::Materialization));
    assert!(root_operation(&plan).children[0].facts().temporary_table.is_none());
    assert!(!culprit_core::insights::insights(&plan).iter().any(|insight| insight.rule == "temp-table-sort"));
    let union = r#"{"query_block":{"union_result":{"using_temporary_table":true,"query_specifications":[
        {"query_block":{"table":{"table_name":"orders","access_type":"ALL","rows_examined_per_scan":100000,"rows_produced_per_join":100000}}}]}}}"#;
    let plan = parse(union, PlanKind::Estimated, None, Some("shop")).unwrap();
    assert_eq!(root_operation(&plan).facts().temporary_table, Some(PlanTemporaryTableUse::Materialization));
    assert!(!plan.has_warning(PlanWarning::Spill));
}

#[test]
fn maria_temporary_wrapper_stays_unknown_even_when_the_statement_has_a_group_by() {
    let json = r#"{"query_block":{"select_id":1,"filesort":{"sort_key":"orders.category","temporary_table":{
        "table":{"table_name":"orders","access_type":"ALL","rows":100000,"filtered":100}}}}}"#;
    let plan =
        parse(json, PlanKind::Estimated, Some("SELECT category,COUNT(*) FROM orders GROUP BY category"), Some("shop"))
            .unwrap();
    assert_eq!(root_operation(&plan).facts().filesort, Some(true));
    assert_eq!(root_operation(&plan).children[0].facts().temporary_table, Some(PlanTemporaryTableUse::Unknown));
    assert!(root_operation(&plan).children[0].facts().filesort.is_none());
    assert!(!plan.has_warning(PlanWarning::Spill));
    let insight =
        culprit_core::insights::insights(&plan).into_iter().find(|insight| insight.rule == "temp-table-sort").unwrap();
    assert!(insight.detail.contains("purpose is unresolved"));
    assert!(!insight.title.starts_with("Groups"));
}

#[test]
fn maria_materialization_does_not_overwrite_its_inner_temporary_wrapper_or_measured_sort_passes() {
    let json = r#"{"query_block":{"select_id":1,"table":{"table_name":"<derived2>","access_type":"ALL",
        "rows":100000,"filtered":100,"materialized":{"query_block":{"select_id":2,"filesort":{
            "sort_key":"orders.category","r_sort_passes":3,"temporary_table":{"table":{
                "table_name":"orders","access_type":"ALL","rows":100000,"filtered":100}}}}}}}}"#;
    let plan = parse(json, PlanKind::Actual, None, Some("shop")).unwrap();
    assert_eq!(root_operation(&plan).facts().temporary_table, Some(PlanTemporaryTableUse::Materialization));
    let sorting = &root_operation(&plan).children[0].children[0];
    assert_eq!(sorting.facts().filesort, Some(true));
    assert_eq!(sorting.facts().sort_merge_passes, Some(3));
    assert_eq!(sorting.children[0].facts().temporary_table, Some(PlanTemporaryTableUse::Unknown));
    assert!(sorting.warnings.contains(&PlanWarning::Spill));
    assert_eq!(sorting.children[0].facts().sort_merge_passes, None);
}

#[test]
fn mysql_tree_native_operators_keep_temporary_use_separate_from_sorting_and_disk_measurements() {
    for (operator, purpose) in [
        ("Aggregate using temporary table", PlanTemporaryTableUse::Grouping),
        ("Temporary table with deduplication", PlanTemporaryTableUse::DuplicateRemoval),
        ("Materialize", PlanTemporaryTableUse::Materialization),
    ] {
        let text = format!(
            "-> {operator}  (actual time=0.1..0.2 rows=100000 loops=1)\n    -> Table scan on orders  (cost=100 rows=100000) (actual time=0.0..0.1 rows=100000 loops=1)"
        );
        let plan = crate::plan_tree::parse(&text, None, Some("shop")).unwrap();
        assert_eq!(plan.root.facts().temporary_table, Some(purpose));
        assert!(plan.root.facts().filesort.is_none());
        assert!(!plan.has_warning(PlanWarning::Spill));
    }
    let text = "-> Sort: orders.category  (cost=100 rows=100000) (actual time=0.1..0.2 rows=100000 loops=1)\n    -> Materialize  (actual time=0.0..0.1 rows=100000 loops=1)";
    let plan = crate::plan_tree::parse(text, None, Some("shop")).unwrap();
    assert_eq!(plan.root.facts().filesort, Some(true));
    assert!(plan.root.facts().temporary_table.is_none());
    assert_eq!(plan.root.children[0].facts().temporary_table, Some(PlanTemporaryTableUse::Materialization));
    assert!(plan.root.facts().sort_on_disk.is_none());
    assert!(!plan.has_warning(PlanWarning::Spill));
}
