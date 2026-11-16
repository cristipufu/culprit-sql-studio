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
    telemetry::{
        CachedStatementContext, NativePlanIdentity, NativeQueryIdentity, PayloadUnavailable, RetainedActualPlanRequest,
    },
};

/// The existing native prefix bounds the only additional retained SQL; no statement payload is read again.
pub(crate) const MAX_STATEMENT_BYTES: usize = crate::statement_origin::PROVENANCE_PREFIX_CHARACTERS * 4;

pub(crate) struct RetainedPlan {
    /// Native statement-counter context observed alongside the lookup. The XML and these counters are separate
    /// point observations; a checked module token does not prove one execution or counter generation.
    pub context: CachedStatementContext,
    pub query: Option<NativeQueryIdentity>,
    pub native_plan: Option<NativePlanIdentity>,
    pub statement: Payload,
    pub statement_utf16_bytes: Option<usize>,
    pub module_parameters: Option<culprit_core::telemetry::MssqlModuleParameters>,
    pub payload: Payload,
    pub source_utf16_bytes: Option<usize>,
    pub executions: String,
    pub last_start_without_offset: Option<String>,
}
pub(crate) struct Read {
    pub setting_on: Option<bool>,
    pub plan: Option<RetainedPlan>,
}
pub(super) fn validate_request(request: &RetainedActualPlanRequest) -> Result<()> {
    requested_ids(request).map(|_| ())
}
fn requested_ids(request: &RetainedActualPlanRequest) -> Result<(i64, Option<i64>)> {
    request.validate()?;
    let query_id = super::numeric(&request.query_id)?.parse::<i64>().map_err(|_| super::ids_are_numbers())?;
    let plan_id = request
        .plan_id
        .as_deref()
        .map(|id| super::numeric(id)?.parse::<i64>().map_err(|_| super::ids_are_numbers()))
        .transpose()?;
    Ok((query_id, plan_id))
}
pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    request: &RetainedActualPlanRequest,
    maximum_bytes: usize,
) -> Result<Read> {
    let (query_id, plan_id) = requested_ids(request)?;
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
        [row] => {
            let mut plan = decode(row, maximum_bytes, MAX_STATEMENT_BYTES)?;
            let query = crate::query_store::decode::prefixed_query_identity(row, "query_store_")?;
            let native_plan = crate::query_store::decode::plan_identity_for_query(row, "query_store_", query.clone())?
                .ok_or_else(invalid_data)?;
            if request.expected_query.as_ref().is_some_and(|expected| expected != &query)
                || request.expected_plan.as_ref().is_some_and(|expected| expected != &native_plan)
                || query.query_id != query_id.to_string()
                || plan_id.is_some_and(|selected| native_plan.plan_id != selected.to_string())
            {
                return Err(culprit_core::telemetry::CollectionStop::SourceChanged.error());
            }
            plan.query = Some(query);
            plan.native_plan = Some(native_plan);
            Some(plan)
        }
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
            plan.module_parameters = None;
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
pub(crate) fn decode(
    row: &crate::value::Rec,
    maximum_bytes: usize,
    maximum_statement_bytes: usize,
) -> Result<RetainedPlan> {
    let bytes =
        optional_integer(row, "plan_bytes")?.map(|value| value.try_into().map_err(|_| invalid_data())).transpose()?;
    let context = cached_statements::context(row)?;
    let prefix = optional_text(row, "text_prefix")?;
    let mut statement = cached_statements::native_statement_from_prefix(&context, prefix.as_deref());
    let statement_utf16_bytes = statement.text().map(|text| text.encode_utf16().count() * 2);
    if statement.text().is_some_and(|text| text.len() > maximum_statement_bytes) {
        statement = Payload::Unavailable(PayloadUnavailable::Oversized);
    }
    let payload = Payload::read_column(row, "query_plan", bytes, maximum_bytes)?;
    let module_parameters =
        if payload.text().is_some() { crate::relational_inputs::parameters(row, &context)? } else { None };
    let plan = RetainedPlan {
        context,
        query: None,
        native_plan: None,
        statement,
        statement_utf16_bytes,
        module_parameters,
        payload,
        source_utf16_bytes: bytes,
        executions: integer(row, "execution_count")?.to_string(),
        last_start_without_offset: optional_text(row, "last_start_without_offset")?
            .map(|at| crate::sql_time::native_time(&at))
            .transpose()?,
    };
    Ok(plan)
}
fn sql() -> String {
    let projection = projection(cached_statements::context_columns(), "st", "st.encrypted", "@P3");
    format!(
        r#"SELECT TOP(1) {projection},{query_identity},{plan_identity}
{relation}
JOIN sys.query_store_query q ON q.query_hash=qs.query_hash AND q.context_settings_id=qs.statement_context_id
JOIN sys.query_store_query_text qt ON qt.query_text_id=q.query_text_id AND qt.statement_sql_handle=qs.statement_sql_handle
JOIN sys.query_store_plan p ON p.query_id=q.query_id AND p.query_plan_hash=qs.query_plan_hash
OUTER APPLY sys.dm_exec_query_plan_stats(qs.plan_handle) ps
WHERE {database} AND q.object_id=COALESCE(st.objectid,0) AND q.query_id=@P1 AND (@P2=-1 OR p.plan_id=@P2)
ORDER BY qs.last_execution_time DESC,qs.creation_time DESC,qs.statement_start_offset OPTION(MAXDOP 1)"#,
        query_identity = crate::query_store::catalog_sql::query_identity_columns!("q.query_id", "q", "query_store_"),
        plan_identity = crate::query_store::catalog_sql::plan_identity_columns!("p", "query_store_"),
        relation = cached_statements::relation(),
        database = cached_statements::DATABASE_PREDICATE,
    )
}
pub(crate) fn projection(context: String, text_alias: &str, encrypted: &str, maximum: &str) -> String {
    let payload =
        crate::cached_payload::columns("CONVERT(nvarchar(max),ps.query_plan)", "plan_bytes", "query_plan", maximum);
    let module = crate::relational_inputs::module_columns(text_alias);
    format!(
        "{context},{module},{encrypted} AS encrypted,{payload},qs.execution_count,CONVERT(varchar(40),qs.last_execution_time,126) AS last_start_without_offset"
    )
}

#[cfg(test)]
mod tests;
