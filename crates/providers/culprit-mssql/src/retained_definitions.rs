//! All-cache last-known plans reuse the interactive retained reader's projection, decoding and setting semantics.
use crate::{
    actual::{self, Availability, retained},
    cached_payload, cached_statements,
    read::{NativeRead, invalid_data},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
const RECORD_METADATA_BYTES: usize = 16 * 1024;
const JSON_BYTES_PER_UTF16_BYTE: usize = 3;
pub(crate) struct Layout {
    pub maximum_bytes: usize,
    pub database_setting: Option<bool>,
}
pub(crate) async fn layout(reader: &mut impl NativeRead, budget: &CollectionBudget) -> Result<Option<Layout>> {
    let maximum_bytes = (budget.page_bytes as usize)
        .checked_sub(RECORD_METADATA_BYTES)
        .ok_or_else(|| CollectionStop::ByteLimit.error())?
        / JSON_BYTES_PER_UTF16_BYTE;
    let maximum_bytes = maximum_bytes.min(budget.max_plan_bytes as usize);
    if maximum_bytes == 0 {
        return Err(CollectionStop::ByteLimit.error());
    }
    match actual::Layout::read(reader).await?.last_actual {
        Availability::Absent => return Ok(None),
        Availability::Hidden => return Err(crate::catalog_visibility::unavailable()),
        Availability::Available => {}
    }
    Ok(Some(Layout { maximum_bytes, database_setting: retained::setting(reader).await? }))
}
pub(crate) fn select_sql(layout: &Layout) -> String {
    let selected = cached_statements::selected_statements();
    let context = cached_statements::context_columns_from("qs", "qs", "qs.compilation_database_id");
    let projection = retained::projection(context, "qs.encrypted", &layout.maximum_bytes.to_string());
    format!(
        "WITH selected AS ({selected}) SELECT {projection} FROM selected qs OUTER APPLY sys.dm_exec_query_plan_stats(qs.plan_handle) ps OPTION(MAXDOP 1)"
    )
}
pub(crate) fn decode(row: &Rec, layout: &Layout) -> Result<MssqlRetainedPlanDefinition> {
    let native = retained::decode(row, layout.maximum_bytes)?;
    let measured = native.context.identity.has_plan_handle();
    let value = MssqlRetainedPlanDefinition {
        context: native.context,
        statement_executions: measured.then_some(native.executions),
        last_start_without_offset: native.last_start_without_offset,
        database_last_query_plan_stats: layout.database_setting,
        batch_plan: cached_payload::from_raw(
            native.payload,
            native.source_utf16_bytes,
            layout.maximum_bytes,
            DefinitionPayload::plan,
        )?,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) mod tests;
