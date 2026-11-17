//! One native waiting-task projection/decoder. Original request facts are never rewritten from task waits.
//! Permissions and native fields: https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-waiting-tasks-transact-sql
use super::*;
use crate::{
    activity::{ActivityRecord, NativeStatement},
    read::{optional_counter, optional_signed_integer, optional_text},
    value::{Rec, Val},
};

const FIELDS: &[(&str, &str, &str)] = &[
    ("task_address", "varchar(16)", "CONVERT(varchar(16),w.waiting_task_address,2)"),
    ("task_session_id", "smallint", "w.session_id"),
    ("task_request_id", "int", "task.request_id"),
    ("task_execution_context_id", "int", "w.exec_context_id"),
    ("task_wait_type", "nvarchar(60)", "w.wait_type"),
    ("task_wait_duration_ms", "bigint", "w.wait_duration_ms"),
    ("task_resource_description", "nvarchar(3072)", "w.resource_description"),
    ("task_resource_address", "varchar(16)", "CONVERT(varchar(16),w.resource_address,2)"),
    ("task_blocking_address", "varchar(16)", "CONVERT(varchar(16),w.blocking_task_address,2)"),
    ("task_blocking_session_id", "smallint", "w.blocking_session_id"),
    ("task_blocking_execution_context_id", "int", "w.blocking_exec_context_id"),
];

pub(super) fn projection() -> String {
    FIELDS.iter().map(|(name, _, expression)| format!("{expression} AS {name}")).collect::<Vec<_>>().join(",")
}

pub(super) fn gated_projection(predicate: &str) -> String {
    FIELDS
        .iter()
        .map(|(name, _, expression)| format!("CASE WHEN {predicate} THEN {expression} END AS {name}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn empty_projection() -> String {
    FIELDS.iter().map(|(name, kind, _)| format!("CAST(NULL AS {kind}) AS {name}")).collect::<Vec<_>>().join(",")
}

pub(super) struct Population {
    pub frame: ActivityFrame,
    pub tasks: Vec<Option<MssqlPageTaskWait>>,
    pub limited: bool,
    pub task_visibility: Option<bool>,
}

impl Population {
    /// The source keeps its original broad-read limit/visibility facts. Unrelated or changed actors cannot
    /// consume a page slot, SQL payload or index lookup justified by a different original capture.
    pub(super) fn retain_requests(&mut self, requests: &[ActivityRequestSelector], database_id: u32) {
        let selectors =
            requests.iter().map(|request| (&request.key, &request.statement_identity)).collect::<BTreeMap<_, _>>();
        let records = std::mem::take(&mut self.frame.records);
        let tasks = std::mem::take(&mut self.tasks);
        for (record, task) in records.into_iter().zip(tasks) {
            if record.facts.scope != ActivityScopeBasis::RequestDatabase
                || selectors.get(&record.facts.key).is_none_or(|identity| {
                    record.facts.statement_identity.as_ref() != Some(*identity)
                        || record.current_locator.as_ref().is_none_or(|locator| {
                            u32::try_from(locator.database).ok() != Some(database_id)
                                || locator.identity() != **identity
                        })
                })
            {
                continue;
            }
            self.frame.records.push(record);
            self.tasks.push(task);
        }
    }
}

pub(super) fn decode(row: &Rec, at: &str) -> Result<Option<MssqlPageTaskWait>> {
    let Some(task_address) = optional_text(row, "task_address")? else {
        if FIELDS.iter().any(|(name, _, _)| !matches!(super::super::rows::value(row, name), Ok(Val::Null))) {
            return Err(invalid_data());
        }
        return Ok(None);
    };
    let wait_type = crate::read::text(row, "task_wait_type")?;
    let native_resource = crate::read::text(row, "task_resource_description")?;
    let resource = resource(&wait_type, &native_resource).ok_or_else(invalid_data)?;
    let number = |name| -> Result<u32> { crate::read::integer(row, name)?.try_into().map_err(|_| invalid_data()) };
    let signed = |name| {
        optional_signed_integer(row, name)?.map(|value| i32::try_from(value).map_err(|_| invalid_data())).transpose()
    };
    let task = MssqlPageTaskWait {
        observed_at: at.into(),
        task_address,
        session_id: number("task_session_id")?,
        request_id: number("task_request_id")?,
        execution_context_id: number("task_execution_context_id")?,
        wait_type,
        wait_duration_ms: optional_counter(row, "task_wait_duration_ms")?,
        resource,
        resource_address: optional_text(row, "task_resource_address")?,
        blocking_task_address: optional_text(row, "task_blocking_address")?,
        blocking_session_id: signed("task_blocking_session_id")?,
        blocking_execution_context_id: signed("task_blocking_execution_context_id")?,
    };
    if task.is_valid() { Ok(Some(task)) } else { Err(invalid_data()) }
}

pub(super) fn resource(wait_type: &str, description: &str) -> Option<ObservedWaitResource> {
    let kind = if wait_type.starts_with("PAGELATCH_") {
        WaitResourceKind::MemoryPage
    } else if wait_type.starts_with("PAGEIOLATCH_") {
        WaitResourceKind::PageIo
    } else {
        return None;
    };
    Some(ObservedWaitResource {
        kind,
        native: NativeIdentity::new(description).qualified("source", "sys.dm_os_waiting_tasks.resource_description"),
    })
}

impl<R: NativeRead> ActivityReader<R> {
    pub(super) async fn page_population(&mut self) -> Result<Population> {
        if self.observed {
            return Err(invalid_data());
        }
        self.observed = true;
        let limit = self.options.budget.max_snapshot_rows.min(MAX_PAGE_WAIT_OBSERVATIONS as u32) as usize;
        let rows = self
            .io
            .query(&sql::observation(self.options.identity), vec![Parameter::Integer(limit as i64 + 1)], limit + 1)
            .await
            .map_err(ReadError::into_error)?;
        if rows.len() > limit + 1 {
            return Err(invalid_data());
        }
        let first = rows.first().ok_or_else(invalid_data)?;
        let context = ActivityContext::decode(first, &self.host, self.port, self.options.identity)?;
        let task_visibility = crate::read::boolean(first, "task_visibility")?;
        let mut records = Vec::new();
        let mut tasks = Vec::new();
        let mut limited = false;
        let mut identities = BTreeSet::new();
        for row in &rows {
            let checked = ActivityContext::decode(row, &self.host, self.port, self.options.identity)?;
            if context != checked || crate::read::boolean(row, "task_visibility")? != task_visibility {
                return Err(CollectionStop::SourceChanged.error());
            }
            let Some(record) = super::super::rows::record(row)? else { continue };
            if records.len() == limit {
                limited = true;
                continue;
            }
            let task = decode(row, &context.at)?;
            if task.is_some() && !task_visibility {
                return Err(invalid_data());
            }
            if task.as_ref().is_some_and(|task| {
                record.facts.key.session.id.parse::<u32>().ok() != Some(task.session_id)
                    || record.facts.key.request.as_ref().and_then(|request| request.id.parse::<u32>().ok())
                        != Some(task.request_id)
            }) {
                return Err(invalid_data());
            }
            let identity = (
                record.facts.key.clone(),
                task.as_ref().map(|task| (task.task_address.clone(), task.execution_context_id)),
            );
            if !identities.insert(identity) {
                return Err(invalid_data());
            }
            records.push(record);
            tasks.push(task);
        }
        withhold_conflicting_locators(&mut records);
        Ok(Population {
            frame: ActivityFrame { context, records },
            tasks,
            limited,
            task_visibility: Some(task_visibility),
        })
    }

    pub(super) async fn enrich_page_participants(
        &mut self,
        frame: &mut ActivityFrame,
        keys: &BTreeSet<ActivityKey>,
    ) -> Result<()> {
        let mut unique = BTreeMap::new();
        for record in &frame.records {
            unique.entry(record.facts.key.clone()).or_insert_with(|| record.clone());
        }
        let mut selected = ActivityFrame { context: frame.context.clone(), records: unique.into_values().collect() };
        self.enrich_using_policy(
            &mut selected,
            keys,
            false,
            MAX_SQL_BYTES,
            super::super::sql::QueryPolicy::SerialMetadata,
        )
        .await?;
        for record in &mut frame.records {
            if let Some(enriched) = selected.records.iter().find(|selected| selected.facts.key == record.facts.key) {
                record.current_sql = enriched.current_sql.clone();
            }
        }
        Ok(())
    }
}

fn withhold_conflicting_locators(records: &mut [ActivityRecord]) {
    let mut locators = BTreeMap::new();
    let mut conflicting = BTreeSet::new();
    for record in records.iter() {
        let locator = record.current_locator.as_ref().map(|locator| locator.identity());
        if let Some(previous) = locators.insert(record.facts.key.clone(), locator.clone())
            && previous != locator
        {
            conflicting.insert(record.facts.key.clone());
        }
    }
    for record in records {
        if conflicting.contains(&record.facts.key) {
            record.current_locator = None;
            record.facts.statement_identity = None;
            record.current_sql = NativeStatement::Unavailable(PayloadUnavailable::Missing);
        }
    }
}

#[cfg(test)]
pub(super) fn no_task(row: &Rec) -> Rec {
    let mut fields = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend(FIELDS.iter().map(|(name, _, _)| (*name, Val::Null)));
    fields.push(("task_visibility", Val::Bool(true)));
    Rec::of(&fields)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(super) use tests::{
    buffer_resource as test_buffer_resource, checked as test_checked, replies as test_replies,
    task_row as test_task_row,
};
