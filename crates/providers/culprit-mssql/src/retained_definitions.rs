//! All-cache last-known plans reuse the interactive retained reader's projection, decoding and setting semantics.
use crate::{
    actual::{self, Availability, retained},
    cached_payload, cached_statements,
    read::{NativeRead, invalid_data},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
const RECORD_METADATA_BYTES: usize = 16 * 1024
    + MAX_MSSQL_RELATIONAL_INPUT_PROOF_BYTES
    + MAX_MSSQL_PARAMETER_CONTEXT_BYTES
    + MAX_MSSQL_CURRENT_TYPE_SCHEMA_BYTES;
const JSON_BYTES_PER_UTF16_BYTE: usize = 3;
pub(crate) struct Layout {
    pub maximum_bytes: usize,
    pub maximum_statement_bytes: usize,
    pub database_setting: Option<bool>,
}
pub(crate) async fn layout(reader: &mut impl NativeRead, budget: &CollectionBudget) -> Result<Option<Layout>> {
    let allowance = (budget.page_bytes as usize)
        .checked_sub(RECORD_METADATA_BYTES)
        .ok_or_else(|| CollectionStop::ByteLimit.error())?
        / JSON_BYTES_PER_UTF16_BYTE;
    let maximum_statement_bytes = (budget.max_sql_bytes as usize).min(retained::MAX_STATEMENT_BYTES);
    let maximum_bytes =
        allowance.checked_sub(maximum_statement_bytes).ok_or_else(|| CollectionStop::ByteLimit.error())?;
    let maximum_bytes = maximum_bytes.min(budget.max_plan_bytes as usize);
    if maximum_bytes == 0 {
        return Err(CollectionStop::ByteLimit.error());
    }
    match actual::Layout::read(reader).await?.last_actual {
        Availability::Absent => return Ok(None),
        Availability::Hidden => return Err(crate::catalog_visibility::unavailable()),
        Availability::Available => {}
    }
    Ok(Some(Layout { maximum_bytes, maximum_statement_bytes, database_setting: retained::setting(reader).await? }))
}
#[cfg(test)]
pub(crate) fn select_sql(layout: &Layout) -> String {
    select_projection(layout, cached_statements::acquisition_frame::Projection::Statement)
}
pub(crate) fn acquisition_sql(layout: &Layout) -> String {
    select_projection(layout, cached_statements::acquisition_frame::Projection::AcquisitionFrame)
}
fn select_projection(layout: &Layout, frame: cached_statements::acquisition_frame::Projection) -> String {
    let selected = cached_statements::selected_statements();
    let context = frame.context_columns("qs", "qs", "qs.compilation_database_id");
    let projection = retained::projection(context, "qs", "qs.encrypted", &layout.maximum_bytes.to_string());
    let current_types_relation = crate::relational_inputs::type_schema::relation("qs");
    frame.batch(format!(
        "WITH selected AS ({selected}) SELECT {projection} FROM selected qs OUTER APPLY sys.dm_exec_query_plan_stats(qs.plan_handle) ps {current_types_relation} OPTION(MAXDOP 1)"
    ))
}
pub(crate) fn decode(
    row: &Rec,
    layout: &Layout,
    source: Option<&CollectionDescription>,
) -> Result<MssqlRetainedPlanDefinition> {
    let native = retained::decode(row, layout.maximum_bytes, layout.maximum_statement_bytes).inspect_err(|_error| {
        #[cfg(test)]
        diagnostics::report("native", row, source, None, _error);
    })?;
    let evidence = source.filter(|source| MssqlRelationalInputProof::supports_source(source)).and_then(|source| {
        native.payload.text().and_then(|xml| {
            crate::plan_xml::original_statement_evidence(
                xml,
                crate::plan_xml::CachedInputContext {
                    statement: &native.context,
                    module_parameters: native.module_parameters.as_ref(),
                    text: native.statement.text(),
                    origin: crate::plan_xml::PlanInputOrigin::OriginalCapture { source },
                },
            )
        })
    });
    let (bindings, parameter_context) =
        evidence.map(|evidence| (evidence.relational_inputs, evidence.parameter_context)).unwrap_or_default();
    let measured = native.context.identity.has_plan_handle();
    let mut value = MssqlRetainedPlanDefinition {
        context: native.context,
        statement_executions: measured.then_some(native.executions),
        last_start_without_offset: native.last_start_without_offset,
        database_last_query_plan_stats: layout.database_setting,
        statement: cached_payload::from_raw(
            native.statement,
            native.statement_utf16_bytes,
            layout.maximum_statement_bytes,
            DefinitionPayload::sql,
        )
        .inspect_err(|_error| {
            #[cfg(test)]
            diagnostics::report("statement-payload", row, source, None, _error);
        })?,
        module_parameters: native.module_parameters,
        current_type_schema: native.current_type_schema,
        relational_input_proof: None,
        parameter_context_proof: None,
        batch_plan: cached_payload::from_raw(
            native.payload,
            native.source_utf16_bytes,
            layout.maximum_bytes,
            DefinitionPayload::plan,
        )
        .inspect_err(|_error| {
            #[cfg(test)]
            diagnostics::report("plan-payload", row, source, None, _error);
        })?,
    };
    if let Some((source, bindings)) = source.zip(bindings)
        && value.batch_plan.payload.is_available()
        && (value.statement.payload.is_available()
            || bindings.iter().all(|binding| {
                binding.input.kind != culprit_core::plan_facts::RelationalInputKind::TableValuedParameter
            }))
    {
        value.relational_input_proof =
            Some(MssqlRelationalInputProof::for_retained(source, &value, bindings).inspect_err(|_error| {
                #[cfg(test)]
                diagnostics::report("relational-proof", row, Some(source), Some(&value), _error);
            })?);
    }
    if let Some((source, context)) = source.zip(parameter_context)
        && value.batch_plan.payload.is_available()
        && MssqlParameterContextProof::supports_source(source)
    {
        value.parameter_context_proof =
            Some(MssqlParameterContextProof::for_retained(source, &value, context).inspect_err(|_error| {
                #[cfg(test)]
                diagnostics::report("parameter-proof", row, Some(source), Some(&value), _error);
            })?);
    }
    if !value.is_valid() {
        let error = invalid_data();
        #[cfg(test)]
        diagnostics::report("retained-model", row, source, Some(&value), &error);
        return Err(error);
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
pub(crate) mod diagnostics;
