use super::*;
use culprit_core::{Engine, PlanKind};

const RUNTIME: &str = r#"<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1"/></RunTimeInformation>"#;

fn statement(hash: &str, parameters: &str, runtime: &str) -> String {
    format!(
        r#"<StmtSimple QueryHash="{hash}" StatementText="SELECT value FROM dbo.rows" StatementSubTreeCost="1">
<QueryPlan>{parameters}<RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="1">
{runtime}<TableScan><Object Schema="[dbo]" Table="[rows]"/></TableScan>
</RelOp></QueryPlan></StmtSimple>"#
    )
}

fn xml(parameters: &str, runtime: &str) -> String {
    document(&statement("0xA", parameters, runtime))
}

fn document(statements: &str) -> String {
    format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{statements}</Statements></Batch></BatchSequence></ShowPlanXML>"
    )
}

fn read_context(xml: &str) -> PlanParameterContext {
    crate::plan_xml::parse_statement(xml, None).unwrap().facts.parameter_context.unwrap()
}

#[test]
fn selected_native_parameter_types_and_representation_differences_survive_without_new_values_or_hashes() {
    let xml = xml(
        r#"<ParameterList><ColumnReference Column="@customer" ParameterDataType="nvarchar(4000)"
ParameterCompiledValue="N'private-compiled-canary'" ParameterRuntimeValue="N'private-runtime-canary'"/></ParameterList>"#,
        RUNTIME,
    );
    let context = read_context(&xml);
    assert!(context.is_valid());
    let parameter = &context.parameters[0];
    assert_eq!(parameter.name.as_deref(), Some("@customer"));
    assert_eq!(parameter.data_type.as_deref(), Some("nvarchar(4000)"));
    assert_eq!(parameter.compiled, State::Recorded);
    assert_eq!(parameter.runtime, State::Recorded);
    assert_eq!(parameter.comparison, Comparison::DifferentRepresentation);
    let retained = serde_json::to_string(&context).unwrap();
    assert!(!retained.contains("private-") && !retained.contains("sha256"));
}

#[test]
fn exact_native_representation_comparison_does_not_claim_sql_value_equivalence() {
    for (compiled, runtime, expected) in [
        ("(4)", "(4)", Comparison::SameRepresentation),
        ("(4)", "(04)", Comparison::DifferentRepresentation),
        ("NULL", "NULL", Comparison::SameRepresentation),
        ("NULL", "(4)", Comparison::DifferentRepresentation),
    ] {
        let metadata = format!(
            r#"<ParameterList><ColumnReference Column="@p" ParameterCompiledValue="{compiled}" ParameterRuntimeValue="{runtime}"/></ParameterList>"#
        );
        assert_eq!(read_context(&xml(&metadata, RUNTIME)).parameters[0].comparison, expected);
    }
}

#[test]
fn compilation_only_missing_runtime_or_hidden_attributes_never_gain_runtime_comparison_proof() {
    let metadata = r#"<ParameterList><ColumnReference Column="@p" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#;
    let plan = crate::plan_xml::parse_statement(&xml(metadata, ""), None).unwrap();
    assert_eq!(plan.kind, PlanKind::Estimated);
    let context = plan.facts.parameter_context.unwrap();
    assert_eq!(context.parameters[0].runtime, State::Unqualified);
    assert_eq!(context.parameters[0].comparison, Comparison::Unavailable);
    let missing = read_context(&xml(
        r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(4)"/></ParameterList>"#,
        RUNTIME,
    ));
    assert_eq!(missing.parameters[0].runtime, State::NotRecorded);
    assert_eq!(missing.parameters[0].comparison, Comparison::Unavailable);
    for hidden in ["(?)", "N'?'", "[redacted]", "N'[REDACTED]'", "'***'"] {
        let metadata = format!(
            r#"<ParameterList><ColumnReference Column="@p" ParameterCompiledValue="(4)" ParameterRuntimeValue="{hidden}"/></ParameterList>"#
        );
        let observed = read_context(&xml(&metadata, RUNTIME));
        assert_eq!(observed.parameters[0].runtime, State::Redacted);
        assert_eq!(observed.parameters[0].comparison, Comparison::Unavailable);
    }
}

#[test]
fn redacted_native_xml_cannot_turn_two_placeholders_into_equal_original_representations() {
    let original = xml(
        r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#,
        RUNTIME,
    );
    let redacted = culprit_core::redact::redact_native(&original, Engine::Mssql);
    let context = read_context(&redacted);
    assert_eq!(context.parameters[0].compiled, State::Redacted);
    assert_eq!(context.parameters[0].runtime, State::Redacted);
    assert_eq!(context.parameters[0].comparison, Comparison::Unavailable);
    assert_eq!(context.parameters[0].data_type.as_deref(), Some("int"));
}

#[test]
fn duplicate_and_unbound_parameter_rows_preserve_metadata_without_choosing_an_owner() {
    let metadata = r#"<ParameterList>
<ColumnReference Column="@p" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/>
<ColumnReference Column="@p" ParameterCompiledValue="(4)" ParameterRuntimeValue="(4)"/>
<ColumnReference ParameterDataType="int" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/>
<ColumnReference Column="@independent" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/>
</ParameterList>"#;
    let context = read_context(&xml(metadata, RUNTIME));
    assert!(context.is_valid());
    assert_eq!(context.parameters.len(), 4);
    assert!(context.issues.contains(&Issue::DuplicateName));
    assert!(context.issues.contains(&Issue::InvalidMetadata));
    assert!(context.parameters[..3].iter().all(|parameter| parameter.comparison == Comparison::Unavailable));
    assert_eq!(context.parameters[2].name, None);
    assert_eq!(context.parameters[3].comparison, Comparison::DifferentRepresentation);
}

#[test]
fn capped_or_ambiguous_parameter_lists_are_not_complete_comparison_evidence() {
    let parameters: String = (0..=MAX_PLAN_PARAMETERS)
        .map(|index| {
            format!(r#"<ColumnReference Column="@p{index}" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/>"#)
        })
        .collect();
    let context = read_context(&xml(&format!("<ParameterList>{parameters}</ParameterList>"), RUNTIME));
    assert!(context.is_valid());
    assert_eq!(context.parameters.len(), MAX_PLAN_PARAMETERS);
    assert_eq!(context.issues, vec![Issue::ParameterLimit]);
    assert!(context.parameters.iter().all(|parameter| parameter.comparison == Comparison::Unavailable));
    let metadata = r#"<ParameterList><ColumnReference Column="@p" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>
<ParameterList><ColumnReference Column="@q" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#;
    let context = read_context(&xml(metadata, RUNTIME));
    assert!(context.issues.contains(&Issue::InvalidMetadata));
    assert!(context.parameters.iter().all(|parameter| parameter.comparison == Comparison::Unavailable));
}

#[test]
fn another_statement_or_nested_query_plan_cannot_supply_runtime_proof() {
    let metadata = r#"<ParameterList><ColumnReference Column="@p" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#;
    let batch = document(&format!("{}{}", statement("0xA", metadata, RUNTIME), statement("0xB", metadata, "")));
    let plan = crate::plan_xml::parse_statement(&batch, Some("0xB")).unwrap();
    assert_eq!(plan.kind, PlanKind::Estimated);
    assert_eq!(plan.facts.parameter_context.unwrap().parameters[0].runtime, State::Unqualified);
    let nested = format!(r#"{metadata}<UDF><QueryPlan><RelOp>{RUNTIME}</RelOp></QueryPlan></UDF>"#);
    let plan = crate::plan_xml::parse_statement(&xml(&nested, ""), None).unwrap();
    assert_eq!(plan.kind, PlanKind::Actual, "the existing parser exposes any native runtime plan");
    assert_eq!(plan.facts.parameter_context.unwrap().parameters[0].runtime, State::Unqualified);
}

#[test]
fn empty_invalid_runtime_counters_and_oversized_value_representations_remain_unqualified() {
    let metadata = r#"<ParameterList><ColumnReference Column="@p" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#;
    for runtime in [
        "<RunTimeInformation/>".to_string(),
        RUNTIME.replace("ActualRows=\"1\"", "ActualRows=\"NaN\""),
        RUNTIME.replace("ActualExecutions=\"1\"", "ActualExecutions=\"0\""),
        RUNTIME.replace("ActualExecutions=\"1\"", "ActualExecutions=\"0.5\""),
    ] {
        let context = read_context(&xml(metadata, &runtime));
        assert_eq!(context.parameters[0].runtime, State::Unqualified);
        assert_eq!(context.parameters[0].comparison, Comparison::Unavailable);
    }
    let metadata = metadata.replace("(5)", &"x".repeat(MAX_PARAMETER_REPRESENTATION_BYTES + 1));
    let context = read_context(&xml(&metadata, RUNTIME));
    assert_eq!(context.parameters[0].runtime, State::Unqualified);
    assert_eq!(context.parameters[0].comparison, Comparison::Unavailable);
}

#[test]
fn selected_actual_native_cache_verdict_has_no_default_or_cross_statement_fallback() {
    let metadata = r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#;
    for (native, expected) in
        [("true", Some(true)), ("false", Some(false)), ("1", None), ("0", None), ("TRUE", None), ("", None)]
    {
        let original = statement("0xA", metadata, RUNTIME).replacen(
            "<StmtSimple ",
            &format!("<StmtSimple RetrievedFromCache=\"{native}\" "),
            1,
        );
        assert_eq!(read_context(&document(&original)).retrieved_from_cache, expected, "{native}");
        let estimated = original.replace(RUNTIME, "");
        assert_eq!(read_context(&document(&estimated)).retrieved_from_cache, None);
    }
    let first =
        statement("0xA", metadata, RUNTIME).replacen("<StmtSimple ", "<StmtSimple RetrievedFromCache=\"true\" ", 1);
    let second = statement("0xB", metadata, "");
    let selected = crate::plan_xml::parse_statement(&document(&format!("{first}{second}")), Some("0xB")).unwrap();
    assert_eq!(selected.facts().parameter_context.as_ref().unwrap().retrieved_from_cache, None);
    assert_eq!(read_context(&xml(metadata, RUNTIME)).retrieved_from_cache, None);
}

#[test]
fn exact_native_integer_comparison_preserves_lexical_difference_without_values_or_hashes() {
    for (compiled, runtime, equal) in [("(004)", "(+4)", true), ("(4)", "(5)", false), ("(-0)", "(0)", true)] {
        let metadata = format!(
            r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="{compiled}" ParameterRuntimeValue="{runtime}"/></ParameterList>"#
        );
        let context = read_context(&xml(&metadata, RUNTIME));
        assert_eq!(context.parameters[0].integer_values_equal, Some(equal));
        assert_eq!(context.parameters[0].comparison, Comparison::DifferentRepresentation);
        let encoded = serde_json::to_string(&context).unwrap();
        assert!(!encoded.contains(compiled) && !encoded.contains(runtime));
        assert!(!encoded.contains("sha256") && !encoded.contains("hash"));
    }
}

#[test]
fn integer_comparison_and_cache_use_are_withheld_when_original_statement_binding_is_lost() {
    let metadata = r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#;
    let native =
        statement("0xA", metadata, RUNTIME).replacen("<StmtSimple ", "<StmtSimple RetrievedFromCache=\"true\" ", 1);
    let mut context = read_context(&document(&native));
    assert_eq!(context.retrieved_from_cache, Some(true));
    assert_eq!(context.parameters[0].integer_values_equal, Some(false));
    withhold_statement_binding(&mut context);
    assert!(context.is_valid());
    assert_eq!(context.retrieved_from_cache, None);
    assert_eq!(context.parameters[0].integer_values_equal, None);
    assert_eq!(context.parameters[0].comparison, Comparison::Unavailable);
}

#[test]
fn restored_native_cache_verdict_must_match_the_selected_original_even_with_a_metadata_gap() {
    let metadata = r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#;
    let native =
        statement("0xA", metadata, RUNTIME).replacen("<StmtSimple ", "<StmtSimple RetrievedFromCache=\"true\" ", 1);
    let mut plan = crate::plan_xml::parse_statement(&document(&native), None).unwrap();
    let mut context = plan.facts().parameter_context.as_ref().unwrap().clone();
    assert!(restore(&mut plan, &context).is_ok());
    context.retrieved_from_cache = Some(false);
    assert!(restore(&mut plan, &context).is_err());
    context.parameters.clear();
    context.uses.clear();
    context.issues = vec![Issue::MetadataLimit];
    assert!(context.is_valid());
    assert!(restore(&mut plan, &context).is_err());
    context.retrieved_from_cache = Some(true);
    assert!(restore(&mut plan, &context).is_ok());
}

#[test]
fn restored_integer_facts_cannot_claim_a_native_string_or_alias_declaration_is_a_supported_integer() {
    for data_type in ["varchar(10)", "dbo.IntegerAlias", "decimal(10,0)"] {
        let metadata = format!(
            r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="{data_type}" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#
        );
        let mut plan = crate::plan_xml::parse_statement(&xml(&metadata, RUNTIME), None).unwrap();
        let mut context = plan.facts().parameter_context.as_ref().unwrap().clone();
        assert!(context.parameters[0].integer_values_equal.is_none());
        context.parameters[0].integer_values_equal = Some(false);
        assert!(context.is_valid(), "the generic safe fact shape carries no engine-name guess");
        assert!(restore(&mut plan, &context).is_err(), "the native owner must reject known declaration contradictions");
    }
}

#[test]
fn restore_preserves_original_recorded_facts_when_reparsed_values_are_redacted_or_unqualified_but_present() {
    let metadata = r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/></ParameterList>"#;
    let native_xml = xml(metadata, RUNTIME);
    let native = read_context(&native_xml);
    assert_eq!(native.parameters[0].compiled, State::Recorded);
    assert_eq!(native.parameters[0].runtime, State::Recorded);
    let redacted = culprit_core::redact::redact_native(&native_xml, Engine::Mssql);
    let empty = xml(
        r#"<ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="" ParameterRuntimeValue=""/></ParameterList>"#,
        RUNTIME,
    );
    for (xml, expected) in [(&redacted, State::Redacted), (&empty, State::Unqualified)] {
        let mut parsed = crate::plan_xml::parse_statement(xml, None).unwrap();
        assert_eq!(parsed.facts().parameter_context.as_ref().unwrap().parameters[0].compiled, expected);
        assert_eq!(parsed.facts().parameter_context.as_ref().unwrap().parameters[0].runtime, expected);
        restore(&mut parsed, &native).unwrap();
        assert_eq!(parsed.facts().parameter_context.as_ref(), Some(&native));
    }
}
