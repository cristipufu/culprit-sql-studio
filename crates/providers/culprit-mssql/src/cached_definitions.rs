//! Cache SQL and statement estimates share native identity, statement slicing and bounded payload decoding.
use crate::{
    cached_payload, cached_statements,
    read::{invalid_data, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
const RECORD_METADATA_BYTES: usize = 16 * 1024;
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
    let selected = cached_statements::selected_statements();
    let statement = cached_statements::statement_expression("qs", "qs.text");
    let sql =
        cached_payload::columns("body.statement_text", "statement_bytes", "statement_text", &limits.sql.to_string());
    let plan = cached_payload::columns("qp.query_plan", "plan_bytes", "query_plan", &limits.plan.to_string());
    let plan_read = cached_statements::statement_plan_relation("qs", "qp");
    format!(
        r#"WITH selected AS ({selected})
SELECT {context},qs.encrypted,CONVERT(varchar(18),qs.query_plan_hash,1) AS query_plan_hash,{sql},{plan}
FROM selected qs OUTER APPLY(SELECT {statement} AS statement_text) body
{plan_read}
OPTION(MAXDOP 1)"#
    )
}
pub(crate) fn decode(row: &Rec, limits: &Limits) -> Result<MssqlCachedDefinition> {
    let value = MssqlCachedDefinition {
        context: cached_statements::context(row)?,
        query_plan_hash: optional_text(row, "query_plan_hash")?,
        statement: cached_payload::decode(
            row,
            "statement_bytes",
            "statement_text",
            limits.sql,
            DefinitionPayload::sql,
        )?,
        estimated_plan: cached_payload::decode(row, "plan_bytes", "query_plan", limits.plan, DefinitionPayload::plan)?,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) mod tests;
