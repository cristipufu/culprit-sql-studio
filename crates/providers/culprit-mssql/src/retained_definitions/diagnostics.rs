//! Error-only diagnostics for owned native fixtures. Raw SQL/XML, handles, hashes and error prose stay private.
use super::*;
use culprit_core::Error;
use serde_json::{Value, json};

pub(crate) fn report(
    stage: &str,
    row: &Rec,
    source: Option<&CollectionDescription>,
    definition: Option<&MssqlRetainedPlanDefinition>,
    error: &Error,
) {
    eprintln!("owned retained decoder failure: {}", summary(stage, row, source, definition, error));
}

fn summary(
    stage: &str,
    row: &Rec,
    source: Option<&CollectionDescription>,
    definition: Option<&MssqlRetainedPlanDefinition>,
    error: &Error,
) -> Value {
    let context = cached_statements::context(row).ok();
    let raw_text = crate::read::optional_text(row, "text_prefix").ok().flatten();
    let statement = context.as_ref().and_then(|context| {
        let body = cached_statements::native_statement_from_prefix(context, raw_text.as_deref());
        body.text().map(|text| text.encode_utf16().count() * 2)
    });
    let raw_plan = row.str("query_plan");
    let xml = raw_plan.as_deref().and_then(|xml| {
        culprit_core::xml::read(xml, |document| {
            let document = document.ok()?;
            let statements = document.descendants().filter(|node| node.is_element() && node.tag_name().name() == "StmtSimple");
            let matching = statements.clone().filter(|node| {
                node.attribute("QueryHash").zip(context.as_ref().and_then(|context| context.query_hash.as_deref()))
                    .is_some_and(|(native, expected)| native.eq_ignore_ascii_case(expected))
            }).take(2).collect::<Vec<_>>();
            Some(json!({
                "statements_capped_at_two": statements.take(2).count(),
                "hash_matching_statements_capped_at_two": matching.len(),
                "selected_statement_utf16_bytes": matching.first().and_then(|node| node.attribute("StatementText")).map(|text| text.encode_utf16().count() * 2),
                "selected_query_plans_capped_at_two": matching.first().map(|node| node.children().filter(|node| node.is_element() && node.tag_name().name() == "QueryPlan").take(2).count()),
                "has_variant": matching.first().is_some_and(|node| node.children().any(|node| node.is_element() && node.tag_name().name() == "QueryPlan" && node.attribute("QueryVariantID").is_some())),
            }))
        })
    });
    json!({
        "stage": stage,
        "error_code": error.code,
        "stop": CollectionStop::from_error(error),
        "context_valid": context.is_some(),
        "source_present": source.is_some(),
        "source_qualified": source.is_some_and(CollectionDescription::is_partition_qualified),
        "source_supports_parameter_proof": source.is_some_and(MssqlParameterContextProof::supports_source),
        "source_statement_utf16_bytes": context.as_ref().and_then(|value| value.source_text_bytes.as_deref()),
        "source_statement_complete": context.as_ref().map(|value| value.source_text_complete),
        "statement_start_utf16_bytes": context.as_ref().map(|value| value.identity.statement_start_bytes),
        "statement_end_utf16_bytes": context.as_ref().map(|value| value.identity.statement_end_bytes),
        "original_statement_utf16_bytes": statement,
        "raw_plan_utf16_bytes": raw_plan.as_ref().map(|value| value.encode_utf16().count() * 2),
        "native_plan_utf16_bytes": crate::read::optional_counter(row, "plan_bytes").ok().flatten(),
        "xml": xml,
        "retained": definition.map(|value| json!({
            "valid": value.is_valid(),
            "context_valid": value.context.is_valid(),
            "statement_valid": value.statement.is_valid(),
            "plan_valid": value.batch_plan.is_valid(),
            "has_module_parameters": value.module_parameters.is_some(),
            "module_parameters_valid": value.module_parameters.as_ref().is_none_or(|parameters| parameters.is_valid_for(&value.context)),
            "has_relational_proof": value.relational_input_proof.is_some(),
            "has_parameter_proof": value.parameter_context_proof.is_some(),
            "statement_available": value.statement.payload.is_available(),
            "plan_available": value.batch_plan.payload.is_available(),
            "statement_redaction_stable": payload_stable(&value.statement.payload, DefinitionPayload::sql),
            "plan_redaction_stable": payload_stable(&value.batch_plan.payload, DefinitionPayload::plan),
        })),
    })
}

fn payload_stable(
    payload: &DefinitionPayload,
    redact: fn(&str, culprit_core::Engine) -> DefinitionPayload,
) -> Option<bool> {
    match payload {
        DefinitionPayload::Available { text, .. } => Some(&redact(text, culprit_core::Engine::Mssql) == payload),
        DefinitionPayload::Unavailable { .. } => None,
    }
}

#[cfg(test)]
mod tests;
