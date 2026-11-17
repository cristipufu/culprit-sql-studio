use super::*;
use culprit_core::plan_facts::PlanParameterComparison;

fn seek(id: u32, expression: &str) -> String {
    format!(
        r#"<RelOp NodeId="{id}" PhysicalOp="Index Seek" LogicalOp="Index Seek" Parallel="false" EstimateRows="1" EstimatedRowsRead="1">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1000" ActualRowsRead="1000" ActualExecutions="1"/></RunTimeInformation>
<IndexScan><Object Schema="[dbo]" Table="[orders]" Index="[ix_customer]"/>
<SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ"><RangeColumns><ColumnReference Table="[orders]" Column="customer"/></RangeColumns>
<RangeExpressions>{expression}</RangeExpressions></Prefix></SeekKeys></SeekPredicateNew></SeekPredicates></IndexScan></RelOp>"#
    )
}

const DIRECT: &str = r#"<ScalarOperator ScalarString="[@customer]"><Identifier><ColumnReference Column="@customer"/></Identifier></ScalarOperator>"#;

fn context(operators: &str) -> PlanParameterContext {
    let xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple QueryHash="0xA" StatementText="SELECT customer FROM dbo.orders WHERE customer=@customer"><QueryPlan>
<ParameterList><ColumnReference Column="@customer" ParameterDataType="int" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>
<RelOp NodeId="4294967295" PhysicalOp="Concatenation" LogicalOp="Concatenation" EstimateRows="1"><Concat>{operators}</Concat></RelOp>
</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    crate::plan_xml::parse_statement(&xml, Some("0xA")).unwrap().facts.parameter_context.unwrap()
}

#[test]
fn direct_native_equality_seek_binds_the_parameter_to_its_exact_operator_without_values() {
    let observed = context(&seek(7, DIRECT));
    assert!(observed.is_valid());
    assert_eq!(
        observed.uses,
        vec![PlanParameterUse { parameter_index: 0, node_id: 7, kind: PlanParameterUseKind::EqualitySeek }]
    );
    assert_eq!(observed.parameters[0].comparison, PlanParameterComparison::DifferentRepresentation);
    assert!(!serde_json::to_string(&observed).unwrap().contains("ScalarString"));
}

#[test]
fn display_text_conversions_residuals_and_compound_seek_keys_never_become_direct_uses() {
    for operator in [
        seek(7, r#"<ScalarOperator ScalarString="[@customer]"/>"#),
        seek(7, &format!("<ScalarOperator><Convert>{DIRECT}</Convert></ScalarOperator>")),
        seek(7, DIRECT).replace(
            "</IndexScan>",
            "<Predicate><ScalarOperator ScalarString=\"customer&gt;0\"/></Predicate></IndexScan>",
        ),
        seek(7, &format!("{DIRECT}{DIRECT}")),
        seek(7, DIRECT).replace("ScanType=\"EQ\"", "ScanType=\"GE\""),
        seek(7, DIRECT).replace("Column=\"@customer\"", "Column=\"@other\""),
    ] {
        let observed = context(&operator);
        assert!(observed.is_valid());
        assert!(observed.uses.is_empty());
        // A text-only scalar has no native parameter reference; it supplies no positive binding either.
        if operator.contains("<ColumnReference Column=\"@") {
            assert!(observed.issues.contains(&Issue::UnsupportedUse));
        }
    }
}

#[test]
fn duplicate_operator_ids_and_oversized_populations_withhold_all_uses_without_first_entry_selection() {
    let duplicate = context(&format!("{}{}", seek(7, DIRECT), seek(7, DIRECT)));
    assert!(duplicate.uses.is_empty());
    assert!(duplicate.issues.contains(&Issue::AmbiguousUse));
    for count in [MAX_PLAN_PARAMETER_USES, MAX_PLAN_PARAMETER_USES + 1] {
        let operators = (0..count).map(|id| seek(id as u32, DIRECT)).collect::<String>();
        let observed = context(&operators);
        assert!(observed.is_valid());
        if count == MAX_PLAN_PARAMETER_USES {
            assert_eq!(observed.uses.len(), count);
        } else {
            assert!(observed.uses.is_empty());
            assert!(observed.issues.contains(&Issue::UseLimit));
        }
    }
}

#[test]
fn nested_query_plan_and_duplicate_parameter_names_never_supply_a_representative_binding() {
    let observed = context(&format!("<UDF><QueryPlan>{}</QueryPlan></UDF>", seek(7, DIRECT)));
    assert!(observed.uses.is_empty());
    let observed = context(&seek(7, DIRECT).replace("</IndexScan>", &format!("</IndexScan>{DIRECT}")));
    assert!(observed.uses.is_empty());
    assert!(observed.issues.contains(&Issue::UnsupportedUse));
}
