//! Cache SQL and statement estimates share native identity, statement slicing and bounded payload decoding.
use crate::{
    cached_payload, cached_statements,
    read::{invalid_data, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
const RECORD_METADATA_BYTES: usize =
    16 * 1024 + MAX_MSSQL_RELATIONAL_INPUT_PROOF_BYTES + MAX_MSSQL_CURRENT_TYPE_SCHEMA_BYTES;
const JSON_BYTES_PER_UTF16_BYTE: usize = 3;
pub(crate) struct Limits {
    pub sql: usize,
    pub plan: usize,
}
impl Limits {
    pub fn from_budget(budget: &CollectionBudget) -> Result<Self> {
        let allowance = (budget.page_bytes as usize)
            .checked_sub(RECORD_METADATA_BYTES)
            .ok_or_else(|| CollectionStop::ByteLimit.error())?
            / JSON_BYTES_PER_UTF16_BYTE;
        let sql = (budget.max_sql_bytes as usize).min(allowance / 2);
        let plan = (budget.max_plan_bytes as usize).min(allowance - sql);
        if sql == 0 || plan == 0 {
            return Err(CollectionStop::ByteLimit.error());
        }
        Ok(Self { sql, plan })
    }
}
pub(crate) fn select_sql(limits: &Limits) -> String {
    let context = cached_statements::context_columns_from("qs", "qs", "qs.compilation_database_id");
    let module = crate::relational_inputs::module_columns("qs");
    let current_types = crate::relational_inputs::type_schema::columns();
    let current_types_relation = crate::relational_inputs::type_schema::relation("qs");
    let selected = cached_statements::selected_statements();
    let statement = cached_statements::statement_expression("qs", "qs.text");
    let sql =
        cached_payload::columns("body.statement_text", "statement_bytes", "statement_text", &limits.sql.to_string());
    let plan = cached_payload::columns("qp.query_plan", "plan_bytes", "query_plan", &limits.plan.to_string());
    let plan_read = cached_statements::statement_plan_relation("qs", "qp");
    format!(
        r#"WITH selected AS ({selected})
SELECT {context},{module},{current_types},qs.encrypted,CONVERT(varchar(18),qs.query_plan_hash,1) AS query_plan_hash,{sql},{plan}
FROM selected qs OUTER APPLY(SELECT {statement} AS statement_text) body
{plan_read}
{current_types_relation}
OPTION(MAXDOP 1)"#
    )
}
pub(crate) fn decode(
    row: &Rec,
    limits: &Limits,
    source: Option<&CollectionDescription>,
) -> Result<MssqlCachedDefinition> {
    let context = cached_statements::context(row)?;
    let module_parameters = crate::relational_inputs::parameters(row, &context)?;
    let current_type_schema = crate::relational_inputs::type_schema::decode(row, &context, module_parameters.as_ref())?;
    let (statement, statement_bytes) = cached_payload::read(row, "statement_bytes", "statement_text", limits.sql)?;
    let (plan, plan_bytes) = cached_payload::read(row, "plan_bytes", "query_plan", limits.plan)?;
    let bindings = source.filter(|source| MssqlRelationalInputProof::supports_source(source)).and_then(|source| {
        plan.text().and_then(|xml| {
            crate::plan_xml::original_statement_evidence(
                xml,
                crate::plan_xml::CachedInputContext {
                    statement: &context,
                    module_parameters: module_parameters.as_ref(),
                    text: statement.text(),
                    origin: crate::plan_xml::PlanInputOrigin::OriginalCapture { source },
                },
            )
            .and_then(|evidence| evidence.relational_inputs)
        })
    });
    let mut value = MssqlCachedDefinition {
        context,
        query_plan_hash: optional_text(row, "query_plan_hash")?,
        statement: cached_payload::from_raw(statement, statement_bytes, limits.sql, DefinitionPayload::sql)?,
        estimated_plan: cached_payload::from_raw(plan, plan_bytes, limits.plan, DefinitionPayload::plan)?,
        module_parameters,
        current_type_schema,
        relational_input_proof: None,
    };
    if let Some((source, bindings)) = source.zip(bindings)
        && value.estimated_plan.payload.is_available()
        && (value.statement.payload.is_available()
            || bindings.iter().all(|binding| {
                binding.input.kind != culprit_core::plan_facts::RelationalInputKind::TableValuedParameter
            }))
    {
        value.relational_input_proof = Some(MssqlRelationalInputProof::for_cached(source, &value, bindings)?);
    }
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) mod tests;
