//! Explicit current execution-grant evidence over the same activity context, generations and SQL guards as Live.
//! These DMV rows are not an atomic snapshot. A native reservation is not proof that its current statement caused
//! another request's wait, and absent/masked Resource Governor identifiers never become Azure pool identities.
//! https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-exec-query-memory-grants-transact-sql
mod fields;
mod sql;

use super::{ActivityContext, ActivityFrame, ActivityReader, ActivityRecord, ActivityScope, IdentityDetail, rows};
use crate::{
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_counter, optional_text, text},
    value::Rec,
};
use culprit_core::{MeasurementScope, Result, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

const MIN_NATIVE_MAJOR: i64 = 11;
const NATIVE_EDITIONS: &[i64] = &[2, 3, 4, 5, 8];

pub(crate) struct MemoryFrame {
    pub context: ActivityContext,
    pub complete_scope: bool,
    activity: ActivityFrame,
    grants: Vec<ExecutionGrantObservation>,
}
pub(crate) struct MemoryCapture {
    pub context: ActivityContext,
    pub complete_scope: bool,
    pub observations: Vec<ExecutionGrantObservation>,
}
impl MemoryFrame {
    pub fn into_observations(self) -> Result<Vec<ExecutionGrantObservation>> {
        let requests = self
            .activity
            .records
            .into_iter()
            .map(ActivityRecord::into_redacted_observation)
            .map(|request| (request.key.clone(), request))
            .collect::<BTreeMap<_, _>>();
        self.grants
            .into_iter()
            .map(|mut grant| {
                if let Some(before) = &grant.request {
                    let current = requests.get(&before.key).ok_or_else(invalid_data)?.clone();
                    grant.statement_relationship = if matches!(current.statement, DefinitionPayload::Available { .. }) {
                        GrantStatementRelationship::ObservedCurrentStatement
                    } else {
                        GrantStatementRelationship::Unresolved
                    };
                    grant.request = Some(current);
                }
                if !grant.is_valid() {
                    return Err(invalid_data());
                }
                Ok(grant)
            })
            .collect()
    }
}

impl<R: NativeRead> ActivityReader<R> {
    pub(crate) async fn capture_memory(&mut self) -> Result<MemoryCapture> {
        let mut frame = self.observe_memory().await?;
        self.enrich_memory(&mut frame).await?;
        let context = frame.context.clone();
        let complete_scope = frame.complete_scope;
        Ok(MemoryCapture { context, complete_scope, observations: frame.into_observations()? })
    }
    pub(crate) async fn observe_memory(&mut self) -> Result<MemoryFrame> {
        if self.observed || !matches!(self.options.scope, ActivityScope::CurrentDatabase) {
            return Err(invalid_data());
        }
        self.observed = true;
        let initial = self.memory_context().await?;
        if matches!(self.options.identity, IdentityDetail::AzureDatabase) {
            crate::read::require_primary(initial.writable)?;
        }
        let layout = fields::Layout::read(&mut self.io).await?;
        let limit = (self.options.budget.max_snapshot_rows as usize).min(MAX_REQUEST_MEMORY_OBSERVATIONS);
        let native = self
            .io
            .query(
                &sql::observation(&layout, self.options.identity),
                vec![Parameter::Integer((limit + 1) as i64)],
                limit + 1,
            )
            .await
            .map_err(ReadError::into_error)?;
        let context = self.decode_memory_context(native.first().ok_or_else(invalid_data)?)?;
        if !same_owner(&initial, &context) {
            return Err(CollectionStop::SourceChanged.error());
        }
        if native.len() > limit {
            return Err(CollectionStop::RowLimit.error());
        }
        let mut records = Vec::new();
        let mut grants = Vec::new();
        let mut identities = BTreeSet::new();
        for row in &native {
            if context != self.decode_memory_context(row)? {
                return Err(CollectionStop::SourceChanged.error());
            }
            let Some((grant, request)) = decode(row, &context, &layout)? else { continue };
            if !identities.insert(grant.key()?) {
                return Err(invalid_data());
            }
            grants.push(grant);
            if let Some(request) = request {
                records.push(request);
            }
        }
        if grants.len() > limit {
            return Err(CollectionStop::RowLimit.error());
        }
        let activity = ActivityFrame { context: context.clone(), records };
        ActivityEvidence::validate(&activity.observations())?;
        let complete_scope =
            grants.iter().all(|row| row.request_association == RequestGrantAssociation::GenerationChecked);
        Ok(MemoryFrame { context, complete_scope, activity, grants })
    }
    pub(crate) async fn enrich_memory(&mut self, frame: &mut MemoryFrame) -> Result<ActivityDetailStatus> {
        let selected = frame.activity.keys();
        let limit = self.options.budget.max_sql_bytes.min(16 * 1024);
        self.enrich_using_policy(
            &mut frame.activity,
            &selected,
            false,
            limit,
            super::sql::QueryPolicy::MemoryDiagnostic,
        )
        .await
    }
    pub(crate) async fn verify_memory_context(&mut self, initial: &ActivityContext) -> Result<bool> {
        Ok(same_owner(initial, &self.memory_context().await?))
    }
    async fn memory_context(&mut self) -> Result<ActivityContext> {
        let native = self
            .io
            .query(&sql::context(self.options.identity), vec![Parameter::Integer(1)], 1)
            .await
            .map_err(ReadError::into_error)?;
        let [row] = native.as_slice() else { return Err(invalid_data()) };
        self.decode_memory_context(row)
    }
    fn decode_memory_context(&self, row: &Rec) -> Result<ActivityContext> {
        if !NATIVE_EDITIONS.contains(&integer(row, "edition")?) || integer(row, "major")? < MIN_NATIVE_MAJOR {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                culprit_core::t!("collection.unsupported"),
            ));
        }
        ActivityContext::decode(row, &self.host, self.port, self.options.identity)
    }
}

fn same_owner(initial: &ActivityContext, current: &ActivityContext) -> bool {
    initial.database == current.database && initial.epoch == current.epoch && initial.writable == current.writable
}
fn native_time(row: &Rec, name: &str) -> Result<Option<String>> {
    optional_text(row, name)?.map(|value| crate::sql_time::native_time(&value)).transpose()
}
fn raw_handle(row: &Rec, name: &str) -> Result<Option<String>> {
    let Some(value) = optional_text(row, name)? else { return Ok(None) };
    if value.len() > 128 || !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_data());
    }
    Ok(Some(value))
}
fn handle(row: &Rec, name: &str) -> Result<Option<String>> {
    Ok(raw_handle(row, name)?.filter(|value| !value.is_empty() && !value.bytes().all(|byte| byte == b'0')))
}
fn association(row: &Rec, session: &str, request: &str, requested: &str) -> Result<RequestGrantAssociation> {
    let session_id = optional_counter(row, "session_id")?;
    let request_id = optional_counter(row, "request_id")?;
    if session_id.is_none() || request_id.is_none() {
        return Ok(RequestGrantAssociation::MissingRequest);
    }
    if session_id.as_deref() != Some(session) || request_id.as_deref() != Some(request) {
        return Ok(RequestGrantAssociation::ChangedRequest);
    }
    let (Some(login), Some(start), Some(grant_sql), Some(grant_plan), Some(request_sql), Some(request_plan)) = (
        native_time(row, "session_generation")?,
        native_time(row, "request_generation")?,
        handle(row, "grant_sql_handle")?,
        handle(row, "grant_plan_handle")?,
        handle(row, "sql_handle")?,
        handle(row, "request_plan_handle")?,
    ) else {
        return Ok(RequestGrantAssociation::InsufficientEvidence);
    };
    let parse =
        |value: &str| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").map_err(|_| invalid_data());
    let login = parse(&login)?;
    let start = parse(&start)?;
    let requested = parse(requested)?;
    let observed = parse(&text(row, "native_observed_at")?)?;
    if requested > observed || integer(row, "scope_basis")? != 1 {
        return Ok(RequestGrantAssociation::InsufficientEvidence);
    }
    if login > start
        || start > requested
        || !grant_sql.eq_ignore_ascii_case(&request_sql)
        || !grant_plan.eq_ignore_ascii_case(&request_plan)
    {
        return Ok(RequestGrantAssociation::ChangedRequest);
    }
    Ok(RequestGrantAssociation::GenerationChecked)
}

fn decode(
    row: &Rec,
    context: &ActivityContext,
    layout: &fields::Layout,
) -> Result<Option<(ExecutionGrantObservation, Option<ActivityRecord>)>> {
    let Some(session) = optional_counter(row, "grant_session_id")? else { return Ok(None) };
    if session == "0" {
        return Err(invalid_data());
    }
    let request_id = integer(row, "grant_request_id")?.to_string();
    let requested = native_time(row, "grant_requested_without_offset")?.ok_or_else(invalid_data)?;
    let granted = native_time(row, "grant_granted_without_offset")?;
    let request_association = association(row, &session, &request_id, &requested)?;
    let request = if request_association == RequestGrantAssociation::GenerationChecked {
        Some(rows::record(row)?.ok_or_else(invalid_data)?)
    } else {
        None
    };
    let pool = optional_counter(row, "grant_pool_id")?;
    let semaphore = optional_counter(row, "grant_semaphore_id")?;
    let resource = pool.zip(semaphore).map(|(pool, semaphore)| {
        NativeIdentity::new(pool)
            .qualified("resource_semaphore_id", semaphore)
            .qualified("kind", "sql-server-resource-governor")
    });
    let mut grant = ExecutionGrantObservation {
        source: culprit_core::source_id::CURRENT_REQUEST_MEMORY.into(),
        identity: NativeIdentity::new(format!("{session}:{request_id}"))
            .qualified("requested_without_offset", &requested)
            .qualified("native_object", "sys.dm_exec_query_memory_grants"),
        observed_at: context.at.clone(),
        state: if granted.is_some() { ExecutionGrantState::Granted } else { ExecutionGrantState::Waiting },
        request_association,
        statement_relationship: GrantStatementRelationship::Unresolved,
        request: request.as_ref().map(|row| row.facts.clone()),
        scope: request.as_ref().map(|_| MeasurementScope::Database),
        requested_without_offset: requested,
        granted_without_offset: granted,
        requested_kibibytes: None,
        required_kibibytes: None,
        granted_kibibytes: None,
        used_kibibytes: None,
        maximum_used_kibibytes: None,
        ideal_kibibytes: None,
        wait_ms: None,
        timeout_seconds: None,
        degree_of_parallelism: None,
        reserved_workers: None,
        used_workers: None,
        maximum_used_workers: None,
        resource,
        workload_group: optional_counter(row, "grant_group_id")?
            .map(|group| NativeIdentity::new(group).qualified("kind", "sql-server-resource-governor-group")),
        native_properties: Default::default(),
        available_columns: vec!["session_id".into(), "request_id".into(), "request_time".into()],
        native_null_columns: vec![],
    };
    layout.decode(row, &mut grant)?;
    for (name, alias) in
        [("pool_id", "grant_pool_id"), ("group_id", "grant_group_id"), ("resource_semaphore_id", "grant_semaphore_id")]
    {
        if let Some(value) = optional_counter(row, alias)? {
            grant.native_properties.insert(name.into(), value);
        }
    }
    for (name, alias) in [("sql_handle", "grant_sql_handle"), ("plan_handle", "grant_plan_handle")] {
        if let Some(value) = raw_handle(row, alias)? {
            grant.native_properties.insert(name.into(), format!("0x{}", value.to_ascii_uppercase()));
        }
    }
    if !grant.is_valid() {
        return Err(invalid_data());
    }
    Ok(Some((grant, request)))
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
pub(crate) mod tests;
