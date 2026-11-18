//! One selected sealed original supplies structural diagnostics after native shutdown. Values, SQL, XML and
//! native attribute text never enter this output; comparison of redacted text supplies no original binding.
use culprit_core::telemetry::*;
use serde_json::{Value, json};

const MAX_DIAGNOSTIC_BYTES: usize = 4096;

pub(super) fn report(source: &CollectionDescription, definition: &MssqlRetainedPlanDefinition) {
    let diagnostic = describe(source, definition);
    let encoded = serde_json::to_string(&diagnostic).unwrap();
    assert!(encoded.len() <= MAX_DIAGNOSTIC_BYTES);
    eprintln!("owned retained scalar proof diagnostic: {encoded}");
}

fn describe(source: &CollectionDescription, definition: &MssqlRetainedPlanDefinition) -> Value {
    let parsed = crate::plan_xml::parse_retained_definition(definition, source);
    let parameters = parsed.as_ref().ok().and_then(|plan| plan.facts().parameter_context.as_ref());
    json!({
        "source_supports_relational_proof":MssqlRelationalInputProof::supports_source(source),
        "source_supports_parameter_proof":MssqlParameterContextProof::supports_source(source),
        "original_scope_qualified":source.is_partition_qualified(),
        "original_plan_available":definition.batch_plan.payload.is_available(),
        "original_sql_available":definition.statement.payload.is_available(),
        "source_text_complete":definition.context.source_text_complete,
        "source_text_length_present":definition.context.source_text_bytes.is_some(),
        "plan_source_utf16_bytes":definition.batch_plan.source_utf16_bytes,
        "native_query_hash_present":definition.context.query_hash.is_some(),
        "native_plan_handle_present":definition.context.identity.has_plan_handle(),
        "native_sql_handle_present":definition.context.identity.has_sql_handle(),
        "relational_proof_present":definition.relational_input_proof.is_some(),
        "parameter_proof_present":definition.parameter_context_proof.is_some(),
        "module_parameters_present":definition.module_parameters.is_some(),
        "retained_parse_succeeded":parsed.is_ok(),
        "retained_parse_error_code":parsed.as_ref().err().map(|error|error.code),
        "parsed_parameter_context_present":parameters.is_some(),
        "parsed_parameter_count":parameters.map(|context|context.parameters.len()),
        "parsed_parameter_issues":parameters.map(|context|&context.issues),
        "redacted_payload_structure":payload_structure(definition),
        "redacted_text_comparison_is_original_authority":false,
    })
}

fn payload_structure(definition: &MssqlRetainedPlanDefinition) -> Value {
    let DefinitionPayload::Available { text: xml, .. } = &definition.batch_plan.payload else {
        return json!({"payload_available":false});
    };
    culprit_core::xml::read(xml, |document| {
        let Ok(document) = document else { return json!({"xml_readable":false}) };
        let candidates = || document.descendants().filter(|node| crate::plan_xml::query_plan_of(*node).is_some());
        let candidate_count = candidates().take(2).count();
        let relop_count = document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "RelOp")
            .take(2)
            .count();
        let has_runtime =
            document.descendants().any(|node| node.is_element() && node.tag_name().name() == "RunTimeInformation");
        let matching = candidates()
            .filter(|node| {
                definition.context.query_hash.as_deref().is_none_or(|hash| {
                    node.attribute("QueryHash").is_some_and(|native| native.eq_ignore_ascii_case(hash.trim()))
                })
            })
            .take(2)
            .count();
        let Ok(statement) =
            crate::plan_xml::select_native_statement(&document, definition.context.query_hash.as_deref())
        else {
            return json!({"xml_readable":true,"candidate_statement_count_capped_at_two":candidate_count,"matching_statement_count_capped_at_two":matching,"native_selection_succeeded":false,"relop_count_capped_at_two":relop_count,"has_runtime_information":has_runtime});
        };
        let plan = crate::plan_xml::query_plan_of(statement).unwrap();
        let sql = match &definition.statement.payload {
            DefinitionPayload::Available { text, .. } => Some(text.as_str()),
            _ => None,
        };
        json!({
            "xml_readable":true,"candidate_statement_count_capped_at_two":candidate_count,"matching_statement_count_capped_at_two":matching,"native_selection_succeeded":true,
            "relop_count_capped_at_two":relop_count,
            "contains_data_access_operator":plan.descendants().any(|node|node.is_element()&&matches!(node.attribute("PhysicalOp"),Some("Table Scan"|"Index Scan"|"Clustered Index Scan"|"Index Seek"|"Clustered Index Seek"))),
            "selected_statement_is_simple":statement.tag_name().name()=="StmtSimple",
            "query_plan_count_capped_at_two":statement.children().filter(|node|node.is_element()&&node.tag_name().name()=="QueryPlan").take(2).count(),
            "parameter_list_count_capped_at_two":plan.children().filter(|node|node.is_element()&&node.tag_name().name()=="ParameterList").take(2).count(),
            "has_runtime_information":plan.descendants().any(|node|node.is_element()&&node.tag_name().name()=="RunTimeInformation"),
            "statement_text_matches_redacted_sql":sql.is_some()&&statement.attribute("StatementText").map(str::trim)==sql.map(str::trim),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resources::test_support::replace, value::Val};

    fn source() -> CollectionDescription {
        crate::plan_xml::test_source(SnapshotDataset::RetainedPlans)
    }
    fn limits() -> crate::retained_definitions::Layout {
        crate::retained_definitions::Layout {
            maximum_bytes: 128 * 1024,
            maximum_statement_bytes: crate::actual::retained::MAX_STATEMENT_BYTES,
            database_setting: Some(true),
        }
    }

    #[test]
    fn selected_original_diagnostics_keep_structural_states_without_values_sql_xml_or_names() {
        let row = crate::retained_definitions::tests::tvp_row();
        let xml=crate::read::text(&row,"query_plan").unwrap().replace("</QueryPlan>",r#"<ParameterList><ColumnReference Column="@private_parameter_name_canary" ParameterDataType="nvarchar(64)" ParameterCompiledValue="N'private-compiled-canary'" ParameterRuntimeValue="N'private-runtime-canary'"/></ParameterList></QueryPlan>"#);
        let row = replace(
            &row,
            &[("plan_bytes", Val::Int((xml.encode_utf16().count() * 2) as i64)), ("query_plan", Val::Text(xml))],
        );
        let definition = crate::retained_definitions::decode(&row, &limits(), Some(&source())).unwrap();
        let diagnostic = describe(&source(), &definition);
        assert_eq!(diagnostic["source_supports_parameter_proof"], true);
        assert_eq!(diagnostic["original_plan_available"], true);
        assert_eq!(diagnostic["redacted_payload_structure"]["parameter_list_count_capped_at_two"], 1);
        let encoded = serde_json::to_string(&diagnostic).unwrap();
        assert!(encoded.len() <= MAX_DIAGNOSTIC_BYTES);
        for secret in [
            "private-compiled-canary",
            "private-runtime-canary",
            "@private_parameter_name_canary",
            "SELECT id FROM",
            "<ShowPlanXML",
        ] {
            assert!(!encoded.contains(secret));
        }
    }

    #[test]
    fn diagnostics_distinguish_missing_parameter_list_unmatched_native_hash_and_unavailable_payload() {
        let row = crate::retained_definitions::tests::tvp_row();
        let definition = crate::retained_definitions::decode(&row, &limits(), Some(&source())).unwrap();
        let diagnostic = describe(&source(), &definition);
        assert_eq!(diagnostic["redacted_payload_structure"]["native_selection_succeeded"], true);
        assert_eq!(diagnostic["redacted_payload_structure"]["parameter_list_count_capped_at_two"], 0);
        let different = replace(&row, &[("query_hash", Val::Text("0xFEDCBA9876543210".into()))]);
        let different = crate::retained_definitions::decode(&different, &limits(), Some(&source())).unwrap();
        let diagnostic = describe(&source(), &different);
        assert_eq!(diagnostic["redacted_payload_structure"]["native_selection_succeeded"], false);
        assert_eq!(diagnostic["redacted_payload_structure"]["matching_statement_count_capped_at_two"], 0);
        let mut missing = definition;
        missing.relational_input_proof = None;
        missing.parameter_context_proof = None;
        missing.batch_plan.payload = DefinitionPayload::Unavailable { reason: PayloadUnavailable::SourceUnavailable };
        missing.batch_plan.native_sha256 = None;
        assert!(missing.is_valid());
        let diagnostic = describe(&source(), &missing);
        assert_eq!(diagnostic["original_plan_available"], false);
        assert_eq!(diagnostic["redacted_payload_structure"]["payload_available"], false);
    }
    #[test]
    fn root_only_showplan_without_query_plan_does_not_become_parameter_proof() {
        let row = crate::retained_definitions::tests::tvp_row();
        let xml = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementType="SELECT" StatementText="SELECT id FROM @Rows" QueryHash="0x0123456789ABCDEF"/></Statements></Batch></BatchSequence></ShowPlanXML>"#;
        let row = replace(
            &row,
            &[("plan_bytes", Val::Int((xml.encode_utf16().count() * 2) as i64)), ("query_plan", Val::Text(xml.into()))],
        );
        let definition = crate::retained_definitions::decode(&row, &limits(), Some(&source())).unwrap();
        assert!(definition.parameter_context_proof.is_none());
        let diagnostic = describe(&source(), &definition);
        assert_eq!(diagnostic["original_plan_available"], true);
        assert_eq!(diagnostic["redacted_payload_structure"]["candidate_statement_count_capped_at_two"], 0);
        assert_eq!(diagnostic["redacted_payload_structure"]["relop_count_capped_at_two"], 0);
        assert_eq!(diagnostic["redacted_payload_structure"]["has_runtime_information"], false);
        assert_eq!(diagnostic["parsed_parameter_context_present"], false);
    }
}
