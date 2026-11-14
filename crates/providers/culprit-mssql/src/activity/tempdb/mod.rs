//! Explicit task tempdb allocation/deallocation counters shared by scans and collection. Native request IDs are
//! joined context, not a task incarnation or proof that the displayed statement allocated these pages.
//! https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-views/sys-dm-db-task-space-usage-transact-sql
mod sql;

use super::{
    ActivityContext, ActivityFrame, ActivityReader, ActivityRecord, ActivityScope, NativeStatement,
    participants::{Check, RequestState},
    rows,
};
use crate::{
    read::{
        NativeRead, Parameter, ReadError, boolean, integer, invalid_data, optional_counter, optional_text,
        stops_remaining_reads,
    },
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const MAX_NATIVE_ROWS: usize = 200;
const NATIVE_PAGE_BYTES: u32 = 8192;

pub(crate) struct TaskSpaceCapture {
    pub context: ActivityContext,
    pub snapshot: TaskSpaceSnapshot,
    pub limited: bool,
    pub context_complete: bool,
    pub context_error_code: Option<culprit_core::ErrorCode>,
}

struct RequestChecks {
    rows: BTreeMap<usize, Check>,
    error_code: Option<culprit_core::ErrorCode>,
}

impl<R: NativeRead> ActivityReader<R> {
    pub(crate) async fn capture_task_space(&mut self) -> Result<TaskSpaceCapture> {
        if self.observed || !matches!(self.options.scope, ActivityScope::CurrentDatabase) {
            return Err(invalid_data());
        }
        self.observed = true;
        let initial = self.task_space_context().await?;
        if matches!(self.options.identity, super::IdentityDetail::AzureDatabase) {
            crate::read::require_primary(initial.writable)?;
        }
        let limit = (self.options.budget.max_snapshot_rows as usize).min(MAX_NATIVE_ROWS);
        let native = self
            .io
            .query(&sql::observation(self.options.identity), vec![Parameter::Integer((limit + 1) as i64)], limit + 1)
            .await
            .map_err(ReadError::into_error)?;
        let context = ActivityContext::decode(
            native.first().ok_or_else(invalid_data)?,
            &self.host,
            self.port,
            self.options.identity,
        )?;
        if !same_source(&initial, &context) || initial.at > context.at {
            return Err(CollectionStop::SourceChanged.error());
        }
        let limited = native.len() > limit;
        let mut observations = Vec::new();
        let mut requests = BTreeMap::new();
        let mut identities = BTreeSet::new();
        for row in native.iter().take(limit) {
            if context != ActivityContext::decode(row, &self.host, self.port, self.options.identity)? {
                return Err(CollectionStop::SourceChanged.error());
            }
            let Some((task, request)) = decode(row, &context)? else { continue };
            if !identities.insert(task.task.clone()) {
                return Err(invalid_data());
            }
            if let Some(request) = request {
                let key = request.facts.key.clone();
                if let Some(previous) = requests.get(&key) {
                    // Independently read parallel rows cannot silently select conflicting statement locators.
                    if !same_statement(previous, &request) {
                        return Err(CollectionStop::SourceChanged.error());
                    }
                } else {
                    requests.insert(key, request);
                }
            }
            observations.push(task);
        }
        let mut frame = ActivityFrame { context: context.clone(), records: requests.into_values().collect() };
        ActivityEvidence::validate(&frame.observations())?;
        let indices = (0..frame.records.len()).collect::<Vec<_>>();
        let before = self.task_space_checks(&frame, &indices).await?;
        let selected = before
            .rows
            .iter()
            .filter(|(index, check)| check.request_state(&frame.records[**index]) == RequestState::SameStatement)
            .map(|(index, _)| frame.records[*index].facts.key.clone())
            .collect();
        let details = self
            .enrich_using_policy(
                &mut frame,
                &selected,
                false,
                self.options.budget.max_sql_bytes.min(16 * 1024),
                super::sql::QueryPolicy::SerialMetadata,
            )
            .await?;
        let after = self.task_space_checks(&frame, &indices).await?;
        attach_requests(&mut observations, frame, &before.rows, &after.rows)?;
        let final_context = self.task_space_context().await?;
        if !same_source(&initial, &final_context) || context.at > final_context.at {
            return Err(CollectionStop::SourceChanged.error());
        }
        let detail_error = match details {
            ActivityDetailStatus::Unavailable { code } => Some(code),
            _ => None,
        };
        let context_error_code = before.error_code.or(after.error_code).or(detail_error);
        let context_complete = context_error_code.is_none()
            && observations.iter().all(|row| row.request_context() == TaskRequestContext::CheckedAtBounds);
        let snapshot = TaskSpaceSnapshot {
            source: culprit_core::source_id::TASK_SPACE.into(),
            observed_from: initial.at.clone(),
            observed_until: final_context.at,
            observations,
        };
        if !snapshot.is_valid() {
            return Err(invalid_data());
        }
        Ok(TaskSpaceCapture { context: initial, snapshot, limited, context_complete, context_error_code })
    }

    async fn task_space_checks(&mut self, frame: &ActivityFrame, indices: &[usize]) -> Result<RequestChecks> {
        match self.request_participants(frame, indices).await {
            Ok(rows) => Ok(RequestChecks { rows, error_code: None }),
            Err(error) if stops_remaining_reads(&error) => Err(error),
            Err(error)
                if error.detail.is_none()
                    && matches!(
                        error.code,
                        culprit_core::ErrorCode::PermissionDenied | culprit_core::ErrorCode::Unsupported
                    ) =>
            {
                Ok(RequestChecks { rows: BTreeMap::new(), error_code: Some(error.code) })
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn verify_task_space_context(&mut self, initial: &ActivityContext) -> Result<bool> {
        let after = self.task_space_context().await?;
        Ok(same_source(initial, &after) && initial.at <= after.at)
    }

    async fn task_space_context(&mut self) -> Result<ActivityContext> {
        let native = self
            .io
            .query(&sql::context(self.options.identity), vec![Parameter::Integer(1)], 1)
            .await
            .map_err(ReadError::into_error)?;
        let [row] = native.as_slice() else { return Err(invalid_data()) };
        if ![2, 3, 4, 5, 8].contains(&integer(row, "edition")?) || integer(row, "major")? < 11 {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                culprit_core::t!("collection.unsupported"),
            ));
        }
        let context = ActivityContext::decode(row, &self.host, self.port, self.options.identity)?;
        if !boolean(row, "task_space_permission")? {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::PermissionDenied,
                culprit_core::t!("provider.mssql.task_space.visibility"),
            ));
        }
        Ok(context)
    }
}

fn same_source(before: &ActivityContext, after: &ActivityContext) -> bool {
    before.database == after.database
        && before.epoch == after.epoch
        && before.edition == after.edition
        && before.major == after.major
        && before.writable == after.writable
}

fn same_statement(before: &ActivityRecord, after: &ActivityRecord) -> bool {
    match (&before.current_locator, &after.current_locator) {
        (Some(before), Some(after)) => {
            before.handle == after.handle
                && before.start == after.start
                && before.end == after.end
                && before.database == after.database
        }
        (None, None) => true,
        _ => false,
    }
}

fn decode(row: &Rec, context: &ActivityContext) -> Result<Option<(TaskSpaceObservation, Option<ActivityRecord>)>> {
    let Some(session) = optional_counter(row, "task_session_id")? else { return Ok(None) };
    let task = TaskSpaceIdentity {
        session_id: session,
        request_id: integer(row, "task_request_id")?.to_string(),
        execution_context_id: integer(row, "task_execution_context_id")?.to_string(),
    };
    let request = if optional_counter(row, "session_id")?.as_deref() == Some(task.session_id.as_str())
        && optional_counter(row, "request_id")?.as_deref() == Some(task.request_id.as_str())
        && optional_text(row, "session_generation")?.is_some()
        && optional_text(row, "request_generation")?.is_some()
        && integer(row, "scope_basis")? == 1
    {
        rows::record(row)?
    } else {
        None
    };
    let observation = TaskSpaceObservation {
        source: culprit_core::source_id::TASK_SPACE.into(),
        observed_at: context.at.clone(),
        task,
        native_database_id: integer(row, "task_database_id")?.to_string(),
        bytes_per_page: NATIVE_PAGE_BYTES,
        user_allocated_pages: optional_counter(row, "user_allocated_pages")?,
        user_deallocated_pages: optional_counter(row, "user_deallocated_pages")?,
        internal_allocated_pages: optional_counter(row, "internal_allocated_pages")?,
        internal_deallocated_pages: optional_counter(row, "internal_deallocated_pages")?,
        request: request.as_ref().map(|row| row.facts.clone()),
        before_check: None,
        after_check: None,
        statement_identity: None,
        plan_identity: None,
    };
    if !observation.is_valid() {
        return Err(invalid_data());
    }
    Ok(Some((observation, request)))
}

fn task_check(check: &Check, record: &ActivityRecord) -> TaskRequestCheck {
    TaskRequestCheck {
        observed_at: check.at.clone(),
        state: match check.request_state(record) {
            RequestState::SameStatement => TaskRequestState::SameStatement,
            RequestState::Missing => TaskRequestState::Missing,
            RequestState::Changed => TaskRequestState::Changed,
            RequestState::Unverified => TaskRequestState::Unverified,
        },
    }
}

fn attach_requests(
    observations: &mut [TaskSpaceObservation],
    frame: ActivityFrame,
    before: &BTreeMap<usize, Check>,
    after: &BTreeMap<usize, Check>,
) -> Result<()> {
    let requests = frame
        .records
        .iter()
        .enumerate()
        .map(|(index, record)| (record.facts.key.clone(), (index, record)))
        .collect::<BTreeMap<_, _>>();
    for row in observations {
        let Some(request) = &row.request else { continue };
        let (index, record) = requests.get(&request.key).ok_or_else(invalid_data)?;
        row.before_check = before.get(index).map(|check| task_check(check, record));
        row.after_check = after.get(index).map(|check| task_check(check, record));
        let mut current = (*record).clone();
        if row.request_context() == TaskRequestContext::CheckedAtBounds {
            if matches!(current.current_sql, NativeStatement::Available(_))
                && let (Some(before), Some(after)) = (before.get(index), after.get(index))
                && let Some((statement, plan)) = after.identities(before)
            {
                row.statement_identity = Some(statement);
                row.plan_identity = plan;
            }
        } else {
            current.current_sql = NativeStatement::Unavailable(PayloadUnavailable::Missing);
        }
        row.request = Some(current.into_redacted_observation());
    }
    Ok(())
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
pub(crate) mod tests;
