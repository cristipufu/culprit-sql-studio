//! PSP children are prepared cache entries. The actual statement's native handle/context selects their exact
//! cache incarnation; a module object ID, generated hint text or shape fingerprint cannot replace that link.
use super::*;
use crate::read::{NativeRead, Parameter, ReadError};
use culprit_core::{
    Result,
    telemetry::{CachedStatementContext, CollectionStop},
};

const LOOKUP_ROWS: usize = 2;
fn sql() -> String {
    format!(
        "SELECT TOP(@P1) {context},CONVERT(varchar(130),qs.statement_sql_handle,1) AS statement_sql_handle,qs.statement_context_id,CONVERT(varchar(18),qs.query_plan_hash,1) AS query_plan_hash {relation} WHERE {database} AND CAST(pa.value AS int)=@P2 AND qs.statement_sql_handle=CONVERT(varbinary(64),@P3,1) AND qs.statement_context_id=@P4 AND qs.query_hash=CONVERT(binary(8),@P5,1) AND qs.query_plan_hash=CONVERT(binary(8),@P6,1) OPTION(MAXDOP 1)",
        context = crate::cached_statements::context_columns(),
        relation = crate::cached_statements::relation(),
        database = crate::cached_statements::DATABASE_PREDICATE
    )
}
async fn lookup(
    reader: &mut impl NativeRead,
    database_id: u32,
    actual: &[String],
    original: Option<&CachedStatementContext>,
) -> Result<CachedStatementContext> {
    if database_id == 0 || original.is_some_and(|original| !original.is_valid()) {
        return Err(CollectionStop::InvalidPage.error());
    }
    let Some(identity) = identity::cache_identity(actual) else {
        eprintln!("owned PSP actual cache identity unavailable: {}", identity::cache_identity_availability(actual));
        return Err(CollectionStop::SourceUnavailable.error());
    };
    let rows = reader
        .query(
            &sql(),
            vec![
                Parameter::Integer(LOOKUP_ROWS as i64),
                Parameter::Integer(i64::from(database_id)),
                Parameter::Text(identity.statement_sql_handle.clone()),
                Parameter::Integer(identity.context_settings_id),
                Parameter::Text(identity.query_hash.clone()),
                Parameter::Text(identity.plan_hash.clone()),
            ],
            LOOKUP_ROWS,
        )
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        eprintln!("owned PSP native cache lookup row count={} (one exact row required)", rows.len());
        return Err(
            if rows.is_empty() { CollectionStop::SourceUnavailable } else { CollectionStop::InvalidPage }.error()
        );
    };
    let context = crate::cached_statements::context(row)?;
    if context.database_id != database_id
        || !context.identity.has_plan_handle()
        || !context.identity.has_sql_handle()
        || !crate::read::text(row, "statement_sql_handle")?.eq_ignore_ascii_case(&identity.statement_sql_handle)
        || crate::read::integer(row, "statement_context_id")? != identity.context_settings_id
        || !context.query_hash.as_deref().is_some_and(|value| value.eq_ignore_ascii_case(&identity.query_hash))
        || !crate::read::text(row, "query_plan_hash")?.eq_ignore_ascii_case(&identity.plan_hash)
        || original.is_some_and(|original| {
            original.database_id != context.database_id
                || original.identity.key().unwrap() != context.identity.key().unwrap()
        })
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(context)
}
pub(super) async fn definitions(
    reader: &mut impl NativeRead,
    database_id: u32,
    actual: &[String],
    original: Option<&CachedStatementContext>,
    cancellation: &CollectionCancellation,
) -> Result<Vec<super::super::super::NamedDefinition>> {
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    let context = lookup(reader, database_id, actual, original).await?;
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    Ok(super::super::super::exact_definitions(reader, &context).await)
}
#[cfg(test)]
mod tests;
