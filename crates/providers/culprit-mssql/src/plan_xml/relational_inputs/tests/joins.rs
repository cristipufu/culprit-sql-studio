use super::*;
use culprit_core::{
    Plan,
    telemetry::{MssqlModuleParameters, NativeReadContext},
};

const SQL: &str = "SELECT r.id FROM @Rows r JOIN dbo.Items i ON r.id=i.id";
const ROWS: &str = r#"Table="[@Rows]" Alias="[r]""#;
const ITEMS: &str = r#"Database="[app]" Schema="[dbo]" Table="[Items]" Alias="[i]""#;

fn scan(id: u32, object: &str) -> String {
    format!(
        r#"<RelOp NodeId="{id}" PhysicalOp="Table Scan" LogicalOp="Table Scan" Parallel="0" EstimateRows="100" EstimatedRowsRead="100" EstimatedTotalSubtreeCost="1">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualRowsRead="20000" ActualExecutions="1" ActualEndOfScans="1" /></RunTimeInformation>
<TableScan><Object {object} /></TableScan></RelOp>"#
    )
}

fn join(sql: &str, left: &str, right: &str, declarations: &str) -> String {
    document(&format!(
        r#"{declarations}<StmtSimple StatementText="{sql}" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="3"><QueryPlan>
<RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join" Parallel="0" EstimateRows="100" EstimatedTotalSubtreeCost="3">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="100" ActualExecutions="1" /></RunTimeInformation>
<NestedLoops>{left}{right}</NestedLoops></RelOp></QueryPlan></StmtSimple>"#
    ))
}

fn parse(sql: &str, xml: &str, read: Option<&NativeReadContext>) -> Plan {
    let mut statement = crate::cached_statements::context(&crate::grants::tests::row()).unwrap();
    statement.query_hash = Some("0x0123456789ABCDEF".into());
    let parameters = MssqlModuleParameters {
        database_id: statement.database_id,
        object_id: 1001,
        module_number: 1,
        sql_handle: statement.identity.sql_handle.clone(),
        table_parameters: [("@Rows".into(), vec!["dbo".into(), "RowsType".into()])].into(),
    };
    crate::plan_xml::parse_cached_statement(
        xml,
        crate::plan_xml::CachedInputContext {
            statement: &statement,
            module_parameters: Some(&parameters),
            text: Some(sql),
            origin: crate::plan_xml::PlanInputOrigin::OriginalNative { read },
        },
    )
    .unwrap()
}

fn kinds(plan: &Plan) -> Vec<Kind> {
    plan.root
        .children
        .iter()
        .filter_map(|node| node.facts.relational_input.map(|input| input.qualified_kind()))
        .collect()
}

#[test]
fn original_module_and_captured_database_bind_a_tvp_beside_an_exact_named_join_input() {
    let xml = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    let read = crate::plan_xml::test_read_context("app");
    let plan = parse(SQL, &xml, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    let input = &plan.root.children[0];
    assert_eq!(input.facts.relational_input.unwrap().provenance, Provenance::NativeParameterMetadata);
    assert_eq!(input.facts.access_rows.unwrap().actual_total, Some(20000.0));
    assert_eq!(input.act_rows, Some(10000.0));
    assert!(input.facts.scan_completion.as_ref().unwrap().is_single_completed_serial_scan());
    let insights = culprit_core::insights::insights(&plan);
    assert!(insights.iter().any(|insight| insight.rule == "misestimate"));
    assert!(
        insights.iter().all(|insight| !insight.detail.contains(&culprit_core::t!(
            "insight.relational_input.completed_input_counts",
            estimated = "100",
            actual = "20K"
        ))),
        "a completed scan under JOIN does not establish the entire TVP input population"
    );
    assert_eq!(kinds(&parse(SQL, &xml, None)), vec![Kind::Unknown]);
    assert_eq!(kinds(&parse(SQL, &xml, Some(&crate::plan_xml::test_read_context("other")))), vec![Kind::Unknown]);
}

#[test]
fn closed_all_variable_join_preserves_module_and_local_proofs_without_a_reader_namespace() {
    let sql = "SELECT r.id FROM @Rows r JOIN @Local l ON r.id=l.id";
    let local = r#"<StmtSimple StatementText="DECLARE @Local TABLE(id int)" />"#;
    let xml = join(sql, &scan(1, ROWS), &scan(2, r#"Table="[@Local]" Alias="[l]""#), local);
    assert_eq!(kinds(&parse(sql, &xml, None)), vec![Kind::TableValuedParameter, Kind::LocalTableVariable]);
    let unknown = xml.replace("DECLARE @Local TABLE(id int)", "DECLARE @Local dbo.RowsType");
    assert_eq!(kinds(&parse(sql, &unknown, None)), vec![Kind::TableValuedParameter, Kind::Unknown]);
    let ambiguous = join(sql, &scan(1, ROWS), &scan(2, r#"Table="[@Local]" Alias="[l]""#), &local.repeat(2));
    assert_eq!(kinds(&parse(sql, &ambiguous, None)), vec![Kind::TableValuedParameter, Kind::Unknown]);
}

#[test]
fn explicit_cross_database_names_match_native_objects_without_guessing_default_schema() {
    let read = crate::plan_xml::test_read_context("app");
    let sql = SQL.replace("dbo.Items", "[History].[archive].[Items]");
    let object = ITEMS.replace("[app]", "[History]").replace("[dbo]", "[archive]");
    let xml = join(&sql, &scan(1, ROWS), &scan(2, &object), "");
    assert_eq!(kinds(&parse(&sql, &xml, Some(&read))), vec![Kind::TableValuedParameter]);
    assert_eq!(kinds(&parse(&sql, &xml, None)), vec![Kind::Unknown]);
    let wrong = xml.replace("Database=\"[History]\"", "Database=\"[Other]\"");
    assert_eq!(kinds(&parse(&sql, &wrong, Some(&read))), vec![Kind::Unknown]);
    for unresolved in ["Items", "app..Items"] {
        let sql = SQL.replace("dbo.Items", unresolved);
        let xml = join(&sql, &scan(1, ROWS), &scan(2, ITEMS), "");
        assert_eq!(kinds(&parse(&sql, &xml, Some(&read))), vec![Kind::Unknown], "{unresolved}");
    }
}

#[test]
fn exact_quoted_table_and_alias_spelling_is_shared_with_native_object_decoding() {
    let sql = "SELECT r.id FROM @Rows r JOIN [s]]chema].[I.tems] [i]]tem] ON r.id=[i]]tem].id";
    let object = r#"Database="[app]" Schema="[s]]chema]" Table="[I.tems]" Alias="[i]]tem]""#;
    let read = crate::plan_xml::test_read_context("app");
    let xml = join(sql, &scan(1, ROWS), &scan(2, object), "");
    assert_eq!(kinds(&parse(sql, &xml, Some(&read))), vec![Kind::TableValuedParameter]);
    let wrong_case = xml.replace("Alias=\"[r]\"", "Alias=\"[R]\"");
    assert_eq!(kinds(&parse(sql, &wrong_case, Some(&read))), vec![Kind::Unknown]);
}

#[test]
fn expanded_missing_extra_or_ambiguous_objects_cannot_borrow_an_outer_parameter_kind() {
    let read = crate::plan_xml::test_read_context("app");
    let base = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    for xml in [
        base.replace("Table=\"[Items]\"", "Table=\"[ExpandedBase]\""),
        base.replace("Alias=\"[i]\"", "Alias=\"[expanded]\""),
        base.replace("Schema=\"[dbo]\"", "Schema=\"[other]\""),
        base.replace("Database=\"[app]\"", ""),
        base.replace("NodeId=\"2\"", "NodeId=\"1\""),
        base.replace("NodeId=\"2\"", ""),
        base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, ROWS))),
        base.replace("</NestedLoops>", &format!("{}</NestedLoops>", scan(3, r#"Table="[@Other]" Alias="[x]""#))),
        join(SQL, &scan(1, ROWS), "", ""),
        base.replace("<Object Database=", "<Object Server=\"[linked]\" Database="),
    ] {
        let plan = parse(SQL, &xml, Some(&read));
        assert_eq!(plan.root.children[0].facts.relational_input.unwrap().qualified_kind(), Kind::Unknown, "{xml}");
    }
}

#[test]
fn healthy_cardinality_and_competing_native_tvf_keep_their_own_evidence() {
    let read = crate::plan_xml::test_read_context("app");
    let base = join(SQL, &scan(1, ROWS), &scan(2, ITEMS), "");
    let healthy = base
        .replace("ActualRows=\"10000\"", "ActualRows=\"100\"")
        .replace("ActualRowsRead=\"20000\"", "ActualRowsRead=\"100\"");
    let plan = parse(SQL, &healthy, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    assert!(!culprit_core::insights::insights(&plan).iter().any(|insight| insight.rule == "misestimate"));
    let sql = SQL.replace("dbo.Items i", "dbo.Items(r.id) i");
    let tvf = scan(2, ITEMS)
        .replace("<TableScan>", "<TableValuedFunction>")
        .replace("</TableScan>", "</TableValuedFunction>");
    let plan = parse(&sql, &join(&sql, &scan(1, ROWS), &tvf, ""), Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::Unknown, Kind::TableValuedFunction]);
}

#[test]
fn a_proven_tvp_does_not_suppress_native_cartesian_evidence_or_establish_full_input_size() {
    let read = crate::plan_xml::test_read_context("app");
    let sql = "SELECT r.id FROM @Rows r CROSS JOIN dbo.Items i";
    let xml = join(sql, &scan(1, ROWS), &scan(2, ITEMS), "")
        .replace("<NestedLoops>", "<Warnings NoJoinPredicate=\"true\" /><NestedLoops>");
    let plan = parse(sql, &xml, Some(&read));
    assert_eq!(kinds(&plan), vec![Kind::TableValuedParameter]);
    assert!(plan.root.warnings.contains(&culprit_core::PlanWarning::NoJoinPredicate));
    let insights = culprit_core::insights::insights(&plan);
    assert!(insights.iter().any(|insight| insight.rule == "no-join-predicate"));
    let full_input =
        culprit_core::t!("insight.relational_input.completed_input_counts", estimated = "100", actual = "20K");
    assert!(insights.iter().all(|insight| !insight.detail.contains(&full_input)));
}
