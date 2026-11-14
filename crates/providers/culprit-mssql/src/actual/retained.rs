//! Retained plan selection owns native scope, payload bounds and metadata; no application statement is executed.
//! Query/plan hashes establish shape, not statement ownership. Match native statement/context handles as well.
//! https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-query-store-query-text-transact-sql
use super::{Availability, Layout};
use crate::{
    cached_statements,
    query_store::payload::Payload,
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_integer, optional_text},
};
use culprit_core::{
    Result,
    telemetry::{CachedStatementContext, PayloadUnavailable},
};

pub(crate) struct RetainedPlan {
    /// Native statement-counter context observed alongside the lookup. The XML and these counters are separate
    /// point observations; a checked module token does not prove one execution or counter generation.
    pub context: CachedStatementContext,
    pub payload: Payload,
    pub source_utf16_bytes: Option<usize>,
    pub executions: String,
    pub last_start_without_offset: Option<String>,
}
pub(crate) struct Read {
    pub setting_on: Option<bool>,
    pub plan: Option<RetainedPlan>,
}
pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    query_id: i64,
    plan_id: Option<i64>,
    maximum_bytes: usize,
) -> Result<Read> {
    let layout = Layout::read(reader).await?;
    match layout.last_actual {
        Availability::Absent => return Err(super::needs_2019()),
        Availability::Hidden => return Err(super::metadata_unavailable()),
        Availability::Available => {}
    }
    let setting_on = setting(reader).await?;
    let rows = reader
        .query(
            &sql(),
            vec![
                Parameter::Integer(query_id),
                Parameter::Integer(plan_id.unwrap_or(-1)),
                Parameter::Integer(maximum_bytes.try_into().map_err(|_| invalid_data())?),
            ],
            2,
        )
        .await
        .map_err(ReadError::into_error)
        .map_err(cache_error)?;
    let mut plan = match rows.as_slice() {
        [] => None,
        [row] => Some(decode(row, maximum_bytes)?),
        _ => return Err(invalid_data()),
    };
    if let Some(plan) = &mut plan
        && plan.payload.text().is_some()
    {
        let mut bindings = crate::relational_inputs::Bindings::default();
        let unavailable = if !bindings.admit(&plan.context)? {
            Some(PayloadUnavailable::NotCollected)
        } else {
            let verified = bindings.verify(reader).await.map_err(cache_error)?;
            (!verified.contains(&plan.context)).then_some(PayloadUnavailable::SourceUnavailable)
        };
        if let Some(reason) = unavailable {
            plan.payload = Payload::Unavailable(reason);
        }
    }
    Ok(Read { setting_on, plan })
}
fn cache_error(error: culprit_core::Error) -> culprit_core::Error {
    if error.code == culprit_core::ErrorCode::PermissionDenied { super::cache_unreadable() } else { error }
}
pub(crate) async fn setting<R: NativeRead>(reader: &mut R) -> Result<Option<bool>> {
    let rows = match reader.query(super::SETTING_SQL, vec![], 2).await {
        Ok(rows) => rows,
        Err(ReadError::Source(error)) if error.code == culprit_core::ErrorCode::PermissionDenied => vec![],
        Err(error) => return Err(error.into_error()),
    };
    match rows.as_slice() {
        [] => Ok(None),
        [row] => Ok(Some(match integer(row, "value")? {
            0 => false,
            1 => true,
            _ => return Err(invalid_data()),
        })),
        _ => Err(invalid_data()),
    }
}
pub(crate) fn decode(row: &crate::value::Rec, maximum_bytes: usize) -> Result<RetainedPlan> {
    let bytes =
        optional_integer(row, "plan_bytes")?.map(|value| value.try_into().map_err(|_| invalid_data())).transpose()?;
    let plan = RetainedPlan {
        context: cached_statements::context(row)?,
        payload: Payload::read_column(row, "query_plan", bytes, maximum_bytes)?,
        source_utf16_bytes: bytes,
        executions: integer(row, "execution_count")?.to_string(),
        last_start_without_offset: optional_text(row, "last_start_without_offset")?
            .map(|at| crate::sql_time::native_time(&at))
            .transpose()?,
    };
    Ok(plan)
}
fn sql() -> String {
    let projection = projection(cached_statements::context_columns(), "st.encrypted", "@P3");
    format!(
        r#"SELECT TOP(1) {projection}
{relation}
JOIN sys.query_store_query q ON q.query_hash=qs.query_hash AND q.context_settings_id=qs.statement_context_id
JOIN sys.query_store_query_text qt ON qt.query_text_id=q.query_text_id AND qt.statement_sql_handle=qs.statement_sql_handle
JOIN sys.query_store_plan p ON p.query_id=q.query_id AND p.query_plan_hash=qs.query_plan_hash
OUTER APPLY sys.dm_exec_query_plan_stats(qs.plan_handle) ps
WHERE {database} AND q.object_id=COALESCE(st.objectid,0) AND q.query_id=@P1 AND (@P2=-1 OR p.plan_id=@P2)
ORDER BY qs.last_execution_time DESC,qs.creation_time DESC,qs.statement_start_offset OPTION(MAXDOP 1)"#,
        relation = cached_statements::relation(),
        database = cached_statements::DATABASE_PREDICATE,
    )
}
pub(crate) fn projection(context: String, encrypted: &str, maximum: &str) -> String {
    let payload =
        crate::cached_payload::columns("CONVERT(nvarchar(max),ps.query_plan)", "plan_bytes", "query_plan", maximum);
    format!(
        "{context},{encrypted} AS encrypted,{payload},qs.execution_count,CONVERT(varchar(40),qs.last_execution_time,126) AS last_start_without_offset"
    )
}

#[cfg(test)]
mod tests;
