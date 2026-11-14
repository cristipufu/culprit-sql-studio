use super::*;

fn statement(operator: &str, parameters: &str) -> String {
    format!(
        r#"<StmtSimple StatementText="SELECT * FROM @rows" QueryHash="0xA" StatementSubTreeCost="10"><QueryPlan>
        <RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="10">
        <RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualExecutions="1" /></RunTimeInformation>
        {operator}</RelOp>{parameters}</QueryPlan></StmtSimple>"#
    )
}

fn document(statements: &str) -> String {
    format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{statements}</Statements></Batch></BatchSequence></ShowPlanXML>"
    )
}

fn input(xml: &str) -> Option<RelationalInput> {
    crate::plan_xml::parse_statement(xml, Some("0xA")).unwrap().root.facts.relational_input
}

#[test]
fn variable_reference_and_parameter_data_type_do_not_distinguish_tvp_from_typed_local() {
    let xml = document(&statement(
        r#"<TableScan><Object Table="[@rows]" /></TableScan>"#,
        r#"<ParameterList><ColumnReference Column="@rows" ParameterDataType="[dbo].[RowsType]" /></ParameterList>"#,
    ));
    assert_eq!(input(&xml), Some(RelationalInput { kind: Kind::Unknown, provenance: Provenance::Unresolved }));
    let plan = crate::plan_xml::parse(&xml).unwrap();
    let concern = culprit_core::insights::insights(&plan)
        .into_iter()
        .find(|insight| insight.rule == "misestimate")
        .expect("recorded estimate mismatch remains useful without knowing input kind");
    assert!(concern.title.contains("100"), "{}", concern.title);
    assert!(!concern.title.contains("Expected 1 row"), "{}", concern.title);
    assert!(concern.fix_sql.is_none());
    assert!(!concern.fix.contains("150"));
}

#[test]
fn a_preceding_inline_declaration_in_the_selected_native_scope_identifies_local_input() {
    let declaration = r#"<StmtSimple StatementText="DECLARE @rows TABLE(id int)" StatementType="DECLARE" />"#;
    let selected = statement(r#"<TableScan><Object Table="[@rows]" /></TableScan>"#, "");
    let xml = document(&format!("{declaration}{selected}"));
    assert_eq!(
        input(&xml),
        Some(RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration })
    );
}

#[test]
fn other_scopes_later_declarations_and_dynamic_text_do_not_classify_the_selected_input() {
    let selected = statement(r#"<TableScan><Object Table="[@rows]" /></TableScan>"#, "");
    let declaration = r#"<StmtSimple StatementText="DECLARE @rows TABLE(id int)" />"#;
    let unrelated = format!("<StmtCond><Then><Statements>{declaration}</Statements></Then></StmtCond>");
    let dynamic = r#"<StmtSimple StatementText="EXEC(N'DECLARE @rows TABLE(id int)')" />"#;
    for statements in
        [format!("{selected}{declaration}"), format!("{unrelated}{selected}"), format!("{dynamic}{selected}")]
    {
        assert_eq!(input(&document(&statements)).unwrap().kind, Kind::Unknown);
    }
    let xml = format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{declaration}</Statements></Batch><Batch><Statements>{selected}</Statements></Batch></BatchSequence></ShowPlanXML>"
    );
    assert_eq!(input(&xml).unwrap().kind, Kind::Unknown);
    let other_name = declaration.replace("@rows", "@different");
    assert_eq!(input(&document(&format!("{other_name}{selected}"))).unwrap().kind, Kind::Unknown);
}

#[test]
fn an_outer_local_declaration_does_not_own_same_spelling_inputs_in_called_or_inlined_code() {
    let declaration = r#"<StmtSimple StatementText="DECLARE @rows TABLE(id int)" />"#;
    let selected = statement(r#"<TableScan><Object Table="[@rows]" /></TableScan>"#, "");
    for sql in ["SELECT * FROM dbo.expand(1)", "SELECT dbo.expand(id) FROM @rows"] {
        let called = selected.replace("SELECT * FROM @rows", sql);
        assert_eq!(input(&document(&format!("{declaration}{called}"))).unwrap().kind, Kind::Unknown, "{sql}");
    }
    let inlined = selected.replace("<QueryPlan>", "<QueryPlan ContainsInlineScalarTsqlUdfs=\"true\">");
    assert_eq!(input(&document(&format!("{declaration}{inlined}"))).unwrap().kind, Kind::Unknown);
}

#[test]
fn internal_relops_and_ambiguous_local_declarations_cannot_borrow_outer_variable_kind() {
    let declaration = r#"<StmtSimple StatementText="DECLARE @rows TABLE(id int)" />"#;
    let selected = statement(
        r#"<TableScan><Object Table="[@rows]" /><InternalInfo><RelOp NodeId="8" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="1"><TableScan><Object Table="[@rows]" /></TableScan></RelOp></InternalInfo></TableScan>"#,
        "",
    );
    let plan = crate::plan_xml::parse_statement(&document(&format!("{declaration}{selected}")), Some("0xA")).unwrap();
    assert_eq!(plan.root.facts.relational_input.unwrap().kind, Kind::Unknown);
    assert_eq!(plan.root.children[0].facts.relational_input.unwrap().kind, Kind::Unknown);
    let direct = statement(r#"<TableScan><Object Table="[@rows]" /></TableScan>"#, "");
    assert_eq!(input(&document(&format!("{declaration}{declaration}{direct}"))).unwrap().kind, Kind::Unknown);
}

#[test]
fn only_native_tvf_or_qualified_tempdb_object_establishes_those_kinds() {
    for (operator, kind) in [
        (
            r#"<TableValuedFunction><Object Schema="[dbo]" Table="[input_rows]" /></TableValuedFunction>"#,
            Kind::TableValuedFunction,
        ),
        (
            r#"<TableScan><Object Database="[tempdb]" Schema="[dbo]" Table="[#rows]" /></TableScan>"#,
            Kind::TemporaryTable,
        ),
        (r#"<TableScan><Object Table="[#rows]" /></TableScan>"#, Kind::Unknown),
    ] {
        assert_eq!(input(&document(&statement(operator, ""))).unwrap().kind, kind);
    }
}

#[test]
fn filter_output_and_repeated_operator_executions_are_not_input_cardinality() {
    let base = document(&statement(r#"<TableScan><Object Table="[@rows]" /></TableScan>"#, ""));
    let filtered = base.replace(
        "ActualRows=\"10000\" ActualExecutions=\"1\"",
        "ActualRows=\"100\" ActualRowsRead=\"10000\" ActualExecutions=\"1\"",
    );
    let repeated = base.replace("ActualExecutions=\"1\"", "ActualExecutions=\"100\"");
    for xml in [filtered, repeated] {
        let plan = crate::plan_xml::parse(&xml).unwrap();
        assert!(!culprit_core::insights::insights(&plan).iter().any(|insight| insight.rule == "misestimate"));
    }
}
