//! Page-specific wait rechecks use the shared native request/statement qualification.
use super::super::{
    ActivityRecord,
    participants::{Check, RequestState},
};
use super::*;
use crate::{
    read::{optional_counter, optional_text},
    value::Rec,
};
use culprit_core::{
    ObservedWaitResource,
    telemetry::{ActivityObservation, PageWaitRecheck, PageWaitRelation, PageWaitRequestState},
};

pub(super) struct CheckedParticipant {
    pub request: Check,
    task: Option<TaskPoint>,
}

struct TaskPoint {
    address: String,
    session: String,
    request: String,
    execution_context: String,
    wait_type: Option<String>,
    resource: Option<String>,
}

impl TaskPoint {
    fn decode(row: &Rec) -> Result<Option<Self>> {
        let Some(address) = optional_text(row, "checked_task_address")? else { return Ok(None) };
        Ok(Some(Self {
            address,
            session: optional_counter(row, "checked_task_session_id")?.ok_or_else(invalid_data)?,
            request: optional_counter(row, "checked_task_request_id")?.ok_or_else(invalid_data)?,
            execution_context: optional_counter(row, "checked_task_execution_context_id")?.ok_or_else(invalid_data)?,
            wait_type: optional_text(row, "checked_task_wait_type")?,
            resource: optional_text(row, "checked_task_resource")?,
        }))
    }

    fn relation(&self, original: &MssqlPageTaskWait) -> PageWaitRelation {
        if self.address != original.task_address
            || self.session.parse::<u32>().ok() != Some(original.session_id)
            || self.request.parse::<u32>().ok() != Some(original.request_id)
            || self.execution_context.parse::<u32>().ok() != Some(original.execution_context_id)
        {
            return PageWaitRelation::Unqualified;
        }
        match self.wait_type.as_deref() {
            None => PageWaitRelation::NoLongerObserved,
            Some(wait) if wait != original.wait_type => PageWaitRelation::Different,
            Some(_) => match self.resource.as_deref() {
                Some(resource) if resource == original.resource.native.id => PageWaitRelation::OriginalObserved,
                Some(_) => PageWaitRelation::Different,
                None => PageWaitRelation::Unqualified,
            },
        }
    }
}

impl CheckedParticipant {
    pub(super) fn recheck(
        &self,
        record: &super::super::ActivityRecord,
        resource: &ObservedWaitResource,
        task: Option<&MssqlPageTaskWait>,
    ) -> PageWaitRecheck {
        let mut checked = self.request.recheck(record, resource);
        if let Some(task) = task
            && checked.request == PageWaitRequestState::SameStatement
        {
            checked.wait = self.task.as_ref().map_or(PageWaitRelation::NoLongerObserved, |point| point.relation(task));
        }
        checked
    }
}

impl<R: NativeRead> ActivityReader<R> {
    pub(super) async fn page_participants(
        &mut self,
        frame: &ActivityFrame,
        indices: &[usize],
        candidates: &[Candidate],
    ) -> Result<BTreeMap<usize, CheckedParticipant>> {
        let tasks = candidates
            .iter()
            .filter_map(|candidate| candidate.task_wait.as_ref().map(|task| (candidate.record, task)))
            .collect::<BTreeMap<_, _>>();
        if tasks.is_empty() {
            return self.request_participants(frame, indices).await.map(|checks| {
                checks
                    .into_iter()
                    .map(|(ordinal, request)| (ordinal, CheckedParticipant { request, task: None }))
                    .collect()
            });
        }
        let mut selectors = String::from("<tasks>");
        for (&ordinal, task) in &tasks {
            selectors.push_str(&format!(
                "<task ordinal=\"{ordinal}\" address=\"{}\" session=\"{}\" request=\"{}\" context=\"{}\"/>",
                task.task_address, task.session_id, task.request_id, task.execution_context_id
            ));
        }
        selectors.push_str("</tasks>");
        let rows = self
            .io
            .query(
                &participants_sql(),
                vec![
                    Parameter::Text(super::super::participants::selectors(frame, indices)?),
                    Parameter::Text(selectors),
                ],
                indices.len(),
            )
            .await
            .map_err(ReadError::into_error)?;
        if rows.len() != indices.len() {
            return Err(invalid_data());
        }
        let mut checked = BTreeMap::new();
        for row in &rows {
            let request = Check::decode(row)?;
            let ordinal = request.ordinal;
            let task = if tasks.contains_key(&ordinal) { TaskPoint::decode(row)? } else { None };
            if !indices.contains(&ordinal) || checked.insert(ordinal, CheckedParticipant { request, task }).is_some() {
                return Err(invalid_data());
            }
        }
        Ok(checked)
    }
}

fn participants_sql() -> String {
    format!(
        r#"
DECLARE /* culprit */ @targets xml=CAST(@P1 AS xml),@tasks xml=CAST(@P2 AS xml),@at datetime2=SYSUTCDATETIME();
SELECT /* culprit */ {columns},
 CONVERT(varchar(16),task.task_address,2) AS checked_task_address,
 task.session_id AS checked_task_session_id,task.request_id AS checked_task_request_id,
 task.exec_context_id AS checked_task_execution_context_id,
 w.wait_type AS checked_task_wait_type,w.resource_description AS checked_task_resource
FROM @targets.nodes('/activity/row') t(n)
CROSS APPLY(SELECT t.n.value('@ordinal[1]','int') AS ordinal,t.n.value('@session[1]','int') AS session_id,
 t.n.value('@request[1]','int') AS request_id) a
LEFT JOIN sys.dm_exec_sessions s ON s.session_id=a.session_id
LEFT JOIN sys.dm_exec_requests r ON r.session_id=a.session_id AND r.request_id=a.request_id
OUTER APPLY(SELECT n.value('@address[1]','varchar(16)') AS address,n.value('@context[1]','int') AS execution_context_id
 FROM @tasks.nodes('/tasks/task') selected(n) WHERE n.value('@ordinal[1]','int')=a.ordinal) selected
LEFT JOIN sys.dm_os_tasks task ON task.task_address=CONVERT(varbinary(8),selected.address,2)
 AND task.session_id=r.session_id AND task.request_id=r.request_id AND task.exec_context_id=selected.execution_context_id
LEFT JOIN sys.dm_os_waiting_tasks w ON w.waiting_task_address=task.task_address
 AND w.session_id=task.session_id AND w.exec_context_id=task.exec_context_id
OPTION(MAXDOP 1,LOOP JOIN)"#,
        columns = super::super::participants::CHECK_COLUMNS
    )
}

impl Check {
    pub(super) fn recheck(&self, record: &ActivityRecord, resource: &ObservedWaitResource) -> PageWaitRecheck {
        self.recheck_request(self.request_state(record), &record.facts, resource)
    }

    pub(super) fn recheck_observation(
        &self,
        original: &ActivityObservation,
        resource: &ObservedWaitResource,
    ) -> PageWaitRecheck {
        self.recheck_request(self.observation_state(original), original, resource)
    }

    fn recheck_request(
        &self,
        state: RequestState,
        original: &ActivityObservation,
        resource: &ObservedWaitResource,
    ) -> PageWaitRecheck {
        let request = match state {
            RequestState::SameStatement => PageWaitRequestState::SameStatement,
            RequestState::Missing => PageWaitRequestState::Missing,
            RequestState::Changed => PageWaitRequestState::Changed,
            RequestState::Unverified => PageWaitRequestState::Unverified,
        };
        let wait = if request == PageWaitRequestState::SameStatement {
            match self.wait_type.as_deref() {
                None => PageWaitRelation::NoLongerObserved,
                Some(native) if Some(native) != original.wait.as_ref().and_then(|wait| wait.native_type.as_deref()) => {
                    PageWaitRelation::Different
                }
                Some(_) => match self.resource.as_deref().filter(|resource| !resource.is_empty()) {
                    Some(current) if current == resource.native.id => PageWaitRelation::OriginalObserved,
                    Some(_) => PageWaitRelation::Different,
                    None => PageWaitRelation::Unqualified,
                },
            }
        } else {
            PageWaitRelation::Unqualified
        };
        PageWaitRecheck { observed_at: self.at.clone(), request, wait }
    }
}
