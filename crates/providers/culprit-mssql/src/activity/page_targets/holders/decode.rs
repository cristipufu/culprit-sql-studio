use super::super::participants::CheckedParticipant;
use super::*;
use crate::{
    read::{boolean, optional_counter, optional_text},
    value::{Rec, Val},
};

pub(super) struct HolderRead {
    pub frame: ActivityFrame,
    pub task: Option<MssqlPageTaskWait>,
    pub visible: bool,
    checked: CheckedParticipant,
    owner_rows: u32,
    owner_task: Option<MssqlPageTaskIdentity>,
    pub owner: Option<ActivityRecord>,
    foreign_database: bool,
}

impl HolderRead {
    pub fn decode(
        rows: &[Rec],
        focus: &PageTaskHolderFocus,
        host: &str,
        port: u16,
        identity: crate::activity::IdentityDetail,
    ) -> Result<Self> {
        if rows.is_empty() || rows.len() >= HOLDER_POINT_ROWS {
            return Err(invalid_data());
        }
        let first = rows.first().ok_or_else(invalid_data)?;
        let context = ActivityContext::decode(first, host, port, identity)?;
        let visible = boolean(first, "task_visibility")?;
        let mut waiter = None;
        let mut owner_rows = 0;
        let mut owner_task = None;
        let mut owner = None;
        let mut foreign_database = false;
        for row in rows {
            if ActivityContext::decode(row, host, port, identity)? != context
                || boolean(row, "task_visibility")? != visible
            {
                return Err(CollectionStop::SourceChanged.error());
            }
            let ordinal = crate::read::integer(row, "ordinal")?;
            match ordinal {
                0 => {
                    let checked = CheckedParticipant::decode(row, true)?;
                    let record = crate::activity::rows::record(row)?;
                    let task = task_waits::decode(row, &context.at)?;
                    if !visible && task.is_some() {
                        return Err(invalid_data());
                    }
                    if let Some((previous, previous_record, previous_task)) = &waiter {
                        let expected: &CheckedParticipant = previous;
                        let previous_record: &Option<ActivityRecord> = previous_record;
                        if expected.request.selector() != checked.request.selector()
                            || expected.recheck_observation(
                                &focus.participant.original,
                                &focus.task.resource,
                                Some(&focus.task),
                            ) != checked.recheck_observation(
                                &focus.participant.original,
                                &focus.task.resource,
                                Some(&focus.task),
                            )
                            || previous_record.as_ref().map(|record| &record.facts)
                                != record.as_ref().map(|record| &record.facts)
                            || !same_task_sample(previous_task, &task)
                        {
                            return Err(CollectionStop::SourceChanged.error());
                        }
                    } else {
                        waiter = Some((checked, record, task));
                    }
                }
                1 => {
                    owner_rows += 1;
                    if !visible || owner_rows > MAX_PAGE_TASK_HOLDER_ROWS {
                        return Err(invalid_data());
                    }
                    let task = task_identity(row)?;
                    let check = Check::decode(row)?;
                    let record = if check.selector().is_some() { crate::activity::rows::record(row)? } else { None };
                    foreign_database |= optional_counter(row, "request_id")?.is_some()
                        && crate::read::integer(row, "scope_basis")? == 5;
                    let user_process = match crate::activity::rows::value(row, "actor_user_process")? {
                        Val::Null => false,
                        _ => boolean(row, "actor_user_process")?,
                    };
                    if owner_rows == 1 {
                        owner = record.filter(|record| {
                            user_process
                                && record.facts.scope == ActivityScopeBasis::RequestDatabase
                                && task.matches_request(&record.facts)
                                && check.selector().is_some_and(|(key, statement)| {
                                    key == record.facts.key
                                        && record.facts.statement_identity.as_ref() == Some(&statement)
                                })
                        });
                        owner_task = Some(task);
                    } else {
                        // A pointer which resolved to multiple native rows is ambiguous, even if their PID or
                        // display SQL happens to agree. Never retain the first owner as a representative.
                        owner = None;
                        owner_task = None;
                    }
                }
                _ => return Err(invalid_data()),
            }
        }
        let (checked, record, task) = waiter.ok_or_else(invalid_data)?;
        if task.as_ref().is_some_and(|task| {
            task.session_id != focus.task.session_id
                || task.request_id != focus.task.request_id
                || task.execution_context_id != focus.task.execution_context_id
        }) {
            return Err(invalid_data());
        }
        Ok(Self {
            frame: ActivityFrame { context, records: record.into_iter().collect() },
            task,
            visible,
            checked,
            owner_rows,
            owner_task,
            owner,
            foreign_database,
        })
    }

    pub fn recheck(&self, focus: &PageTaskHolderFocus) -> PageWaitRecheck {
        let mut checked =
            self.checked.recheck_observation(&focus.participant.original, &focus.task.resource, Some(&focus.task));
        if !self.visible {
            checked.wait = PageWaitRelation::Unqualified;
        }
        checked
    }

    pub fn model_for_page(
        &self,
        focus: &PageTaskHolderFocus,
        page: Option<&PageWaitObservation>,
    ) -> Result<MssqlPageTaskHolderPoint> {
        let mut point = self.model(focus)?;
        if page.is_none() {
            point.holder = MssqlPageTaskHolder {
                relation: PageTaskHolderRelation::Unavailable,
                matching_owner_rows: 0,
                task: None,
                request: None,
            };
        }
        Ok(point)
    }

    pub fn model(&self, focus: &PageTaskHolderFocus) -> Result<MssqlPageTaskHolderPoint> {
        let checked = self.recheck(focus);
        let relation = self.relation(&checked);
        let retain_task = matches!(
            relation,
            PageTaskHolderRelation::QualifiedRequest
                | PageTaskHolderRelation::TaskOnly
                | PageTaskHolderRelation::OtherDatabase
                | PageTaskHolderRelation::Changed
                | PageTaskHolderRelation::SelfReference
        );
        let retain_owner = relation == PageTaskHolderRelation::QualifiedRequest;
        let point = MssqlPageTaskHolderPoint {
            observed_at: self.frame.context.at.clone(),
            database_writable: self.frame.context.writable,
            request: checked.request,
            wait: checked.wait,
            holder: MssqlPageTaskHolder {
                relation,
                matching_owner_rows: if matches!(
                    relation,
                    PageTaskHolderRelation::Sentinel
                        | PageTaskHolderRelation::NotReported
                        | PageTaskHolderRelation::Unavailable
                ) {
                    0
                } else {
                    self.owner_rows
                },
                task: retain_task.then(|| self.owner_task.clone()).flatten(),
                request: retain_owner
                    .then(|| self.owner.clone().map(|record| Box::new(record.into_redacted_observation())))
                    .flatten(),
            },
        };
        if point.holder.is_valid() { Ok(point) } else { Err(invalid_data()) }
    }

    fn relation(&self, checked: &PageWaitRecheck) -> PageTaskHolderRelation {
        let Some(task) = &self.task else { return PageTaskHolderRelation::Unavailable };
        if task.blocking_session_id.is_some_and(|id| id < 0) {
            return PageTaskHolderRelation::Sentinel;
        }
        let Some(address) = &task.blocking_task_address else { return PageTaskHolderRelation::NotReported };
        if address.bytes().all(|byte| byte == b'0') {
            return PageTaskHolderRelation::NotReported;
        }
        if address == &task.task_address {
            return PageTaskHolderRelation::SelfReference;
        }
        if self.owner_rows == MAX_PAGE_TASK_HOLDER_ROWS {
            return PageTaskHolderRelation::Ambiguous;
        }
        if self.owner_rows == 0 {
            return if task.blocking_session_id.is_some_and(|id| id > 0) {
                PageTaskHolderRelation::Changed
            } else {
                PageTaskHolderRelation::Unavailable
            };
        }
        let Some(owner) = &self.owner_task else { return PageTaskHolderRelation::Unavailable };
        if !owner.is_consistent_with_reported_holder(task)
            || checked.request != PageWaitRequestState::SameStatement
            || checked.wait != PageWaitRelation::OriginalObserved
        {
            return PageTaskHolderRelation::Changed;
        }
        if !owner.matches_reported_holder(task) {
            return PageTaskHolderRelation::TaskOnly;
        }
        if self.foreign_database {
            return PageTaskHolderRelation::OtherDatabase;
        }
        if self.owner.is_some() { PageTaskHolderRelation::QualifiedRequest } else { PageTaskHolderRelation::TaskOnly }
    }
}

/// Duplicate join rows may carry different diagnostic suffixes for the same supported page. Every other
/// native task fact must agree; exact unknown raw rows remain structurally consistent without page authority.
fn same_task_sample(previous: &Option<MssqlPageTaskWait>, current: &Option<MssqlPageTaskWait>) -> bool {
    if previous == current {
        return true;
    }
    let (Some(previous), Some(current)) = (previous, current) else { return false };
    if !previous.resource.same_sql_server_page(&current.resource) {
        return false;
    }
    let mut comparable = current.clone();
    comparable.resource = previous.resource.clone();
    previous == &comparable
}

fn task_identity(row: &Rec) -> Result<MssqlPageTaskIdentity> {
    let number =
        |name| optional_counter(row, name)?.map(|value| value.parse::<u32>().map_err(|_| invalid_data())).transpose();
    let task = MssqlPageTaskIdentity {
        task_address: optional_text(row, "holder_task_address")?.ok_or_else(invalid_data)?,
        session_id: number("holder_session_id")?,
        request_id: number("holder_request_id")?,
        execution_context_id: number("holder_execution_context_id")?.ok_or_else(invalid_data)?,
    };
    if task.is_valid() { Ok(task) } else { Err(invalid_data()) }
}
