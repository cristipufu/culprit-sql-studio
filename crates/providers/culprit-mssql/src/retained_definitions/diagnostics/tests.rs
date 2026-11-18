use super::*;
use crate::{resources::test_support::replace, value::Val};

fn limits() -> Layout {
    Layout {
        maximum_bytes: 128 * 1024,
        maximum_statement_bytes: retained::MAX_STATEMENT_BYTES,
        database_setting: Some(true),
    }
}
fn prefix_row() -> Rec {
    let prefix = "CREATE PROC dbo.p @p int AS ";
    let statement = "SELECT @p AS native_payload_length";
    let selected = format!("{prefix}{statement}");
    let full = format!("{selected}; SELECT 99 AS later_statement");
    let xml = format!(
        r#"<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan"><BatchSequence><Batch><Statements><StmtSimple StatementText="{selected}" QueryHash="0x0123456789ABCDEF" StatementType="SELECT"><QueryPlan><ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(1)"/></ParameterList><RelOp NodeId="0" PhysicalOp="Compute Scalar"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1"/></RunTimeInformation></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    replace(
        &crate::cached_definitions::tests::row(),
        &[
            ("statement_start_offset", Val::Int((prefix.encode_utf16().count() * 2) as i64)),
            ("statement_end_offset", Val::Int((selected.encode_utf16().count() * 2 - 2) as i64)),
            ("source_text_bytes", Val::Int((full.encode_utf16().count() * 2) as i64)),
            ("text_prefix", Val::Text(full)),
            ("text_complete", Val::Int(1)),
            ("plan_bytes", Val::Int((xml.encode_utf16().count() * 2) as i64)),
            ("query_plan", Val::Text(xml)),
        ],
    )
}

#[test]
fn a_plan_prefix_shorter_than_the_original_source_withholds_optional_proof_without_invalidating_the_row() {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    assert!(MssqlParameterContextProof::supports_source(&source));
    let definition = decode(&prefix_row(), &limits(), Some(&source)).unwrap();
    assert!(definition.is_valid());
    assert!(definition.statement.payload.is_available());
    assert!(definition.batch_plan.payload.is_available());
    assert!(definition.parameter_context_proof.is_none());
    let record = ObjectDefinitionRecord::new(
        "retained-plan".into(),
        ObjectDefinitionFact::MssqlRetainedPlan(Box::new(definition)),
    )
    .unwrap();
    assert!(record.is_valid());
}

#[test]
fn stage_diagnostics_are_bounded_native_shape_facts_without_raw_bodies_or_error_prose() {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let row = prefix_row();
    let error = culprit_core::Error::new(culprit_core::ErrorCode::QueryFailed, "error-private-canary");
    let report = summary("native", &row, Some(&source), None, &error);
    assert_eq!(report["stage"], "native");
    assert_eq!(report["context_valid"], true);
    assert_eq!(report["source_qualified"], true);
    assert_eq!(report["xml"]["statements_capped_at_two"], 1);
    assert_eq!(report["xml"]["hash_matching_statements_capped_at_two"], 1);
    let encoded = serde_json::to_string(&report).unwrap();
    for private in [
        "error-private-canary",
        "native_payload_length",
        "later_statement",
        "CREATE PROC",
        "0x0123456789ABCDEF",
        "ParameterCompiledValue",
        "(1)",
    ] {
        assert!(!encoded.contains(private));
    }
    assert!(encoded.len() < 4096);
    let invalid = replace(&row, &[("plan_bytes", Val::Int(2))]);
    assert!(decode(&invalid, &limits(), Some(&source)).is_err(), "diagnostics must preserve native length rejection");
}
