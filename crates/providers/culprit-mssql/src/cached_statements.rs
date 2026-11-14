//! Native statement identity, database ownership and provenance shared by cache projections.
mod details;
use crate::{
    read::{integer, invalid_data, optional_boolean, optional_counter, optional_text, signed_integer, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
pub(crate) use details::read as details;

pub(crate) fn relation() -> &'static str {
    "FROM sys.dm_exec_query_stats qs CROSS APPLY sys.dm_exec_plan_attributes(qs.plan_handle) pa OUTER APPLY sys.dm_exec_sql_text(qs.sql_handle) st"
}
pub(crate) const DATABASE_PREDICATE: &str = "pa.attribute='dbid' AND CAST(pa.value AS int)=DB_ID()";
pub(crate) fn context_columns() -> String {
    context_columns_from("qs", "st", "pa.value")
}
pub(crate) fn context_columns_from(cache: &str, text: &str, database: &str) -> String {
    context_columns_with_hash(cache, text, database, true)
}
pub(crate) fn context_columns_without_hash_from(cache: &str, text: &str, database: &str) -> String {
    context_columns_with_hash(cache, text, database, false)
}
fn context_columns_with_hash(cache: &str, text: &str, database: &str, include_hash: bool) -> String {
    let hash =
        if include_hash { format!("CONVERT(varchar(18),{cache}.query_hash,1) AS query_hash,") } else { String::new() };
    format!(
        r#"{cache}.plan_handle,{cache}.sql_handle,{cache}.statement_start_offset,{cache}.statement_end_offset,
 CONVERT(varchar(40),{cache}.creation_time,126) AS creation_time,{cache}.plan_generation_num,
 CAST({database} AS int) AS database_id,{hash}
 CAST(DATALENGTH({text}.text) AS bigint) AS source_text_bytes,
 CASE WHEN {text}.text IS NULL THEN NULL WHEN DATALENGTH({text}.text)<={prefix_bytes} THEN 1 ELSE 0 END AS text_complete,
 SUBSTRING({text}.text,1,{prefix_chars}) AS text_prefix,
 CASE WHEN {text}.dbid=DB_ID() THEN CONVERT(int,OBJECTPROPERTYEX({text}.objectid,N'IsMSShipped')) ELSE NULL END AS engine_internal,
 CAST(NULL AS int) AS cloud_internal"#,
        prefix_chars = crate::statement_origin::PROVENANCE_PREFIX_CHARACTERS,
        prefix_bytes = crate::statement_origin::PROVENANCE_PREFIX_CHARACTERS * 2
    )
}
pub(crate) fn statement_expression(cache: &str, text_expression: &str) -> String {
    format!(
        "SUBSTRING({text_expression},{cache}.statement_start_offset/2+1,(CASE {cache}.statement_end_offset WHEN -1 THEN DATALENGTH({text_expression}) ELSE {cache}.statement_end_offset END-{cache}.statement_start_offset)/2+1)"
    )
}
pub(crate) fn statement_plan_relation(cache: &str, plan: &str) -> String {
    format!(
        "OUTER APPLY sys.dm_exec_text_query_plan({cache}.plan_handle,{cache}.statement_start_offset,{cache}.statement_end_offset) {plan}"
    )
}

/// Select the bounded native statement set before optional plan payload functions are requested.
pub(crate) fn selected_statements() -> String {
    format!(
        "SELECT TOP (@P1) qs.plan_handle,qs.sql_handle,qs.statement_start_offset,qs.statement_end_offset,qs.creation_time,qs.plan_generation_num,qs.query_hash,qs.query_plan_hash,qs.execution_count,qs.last_execution_time,CAST(pa.value AS int) AS compilation_database_id,st.text,st.dbid,st.objectid,st.number,st.encrypted {} WHERE {}",
        relation(),
        DATABASE_PREDICATE
    )
}
pub(crate) fn context(row: &Rec) -> Result<CachedStatementContext> {
    let prefix = optional_text(row, "text_prefix")?;
    let complete = optional_boolean(row, "text_complete")? == Some(true);
    let provenance = crate::statement_origin::read_prefix(row, prefix.as_deref(), complete);
    let value = CachedStatementContext {
        identity: CachedStatementIdentity {
            plan_handle: text(row, "plan_handle")?,
            sql_handle: text(row, "sql_handle")?,
            statement_start_bytes: integer(row, "statement_start_offset")?.try_into().map_err(|_| invalid_data())?,
            statement_end_bytes: signed_integer(row, "statement_end_offset")?.try_into().map_err(|_| invalid_data())?,
            created_without_offset: crate::sql_time::native_time(&text(row, "creation_time")?)?,
            plan_generation: integer(row, "plan_generation_num")?.to_string(),
        },
        database_id: integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?,
        query_hash: optional_text(row, "query_hash")?,
        origin: provenance.origin,
        monitoring_template_id: provenance.template_id,
        source_text_bytes: optional_counter(row, "source_text_bytes")?,
        source_text_complete: complete,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

pub(crate) const MAX_HASH_SELECTORS: usize = 200;
pub(crate) fn valid_hash(hash: &str) -> bool {
    hash.strip_prefix("0x").is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
pub(crate) fn hashes_parameter(hashes: &[String]) -> Result<crate::read::Parameter> {
    if hashes.len() > MAX_HASH_SELECTORS {
        return Err(CollectionStop::RowLimit.error());
    }
    if !hashes.iter().all(|hash| valid_hash(hash)) {
        return Err(invalid_data());
    }
    let mut xml = String::from("<hashes>");
    for hash in hashes {
        xml.push_str(&format!("<h v=\"{hash}\"/>"));
    }
    xml.push_str("</hashes>");
    Ok(crate::read::Parameter::Text(xml))
}
