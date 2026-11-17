//! A native blocking-task pointer is a point relationship. Resolving it to a guarded request neither proves
//! continuous latch ownership nor measures a statement's contribution to contention.
use super::*;

mod focus;
pub use focus::PageTaskHolderFocus;

/// A selected native task address has at most one owner. Two returned rows mean ambiguity, never permission
/// to choose a representative. The source stops retaining owner candidates at this bound.
pub const MAX_PAGE_TASK_HOLDER_ROWS: u32 = 2;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageTaskHolderRelation {
    QualifiedRequest,
    TaskOnly,
    NotReported,
    Sentinel,
    OtherDatabase,
    Changed,
    Ambiguous,
    SelfReference,
    Unavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPageTaskIdentity {
    pub task_address: String,
    pub session_id: Option<u32>,
    pub request_id: Option<u32>,
    pub execution_context_id: u32,
}

impl MssqlPageTaskIdentity {
    pub fn is_valid(&self) -> bool {
        nonzero_address(&self.task_address)
            && self.session_id.is_none_or(|id| id > 0 && id <= i16::MAX as u32)
            && self.request_id.is_none_or(|id| id <= i32::MAX as u32)
            && self.execution_context_id <= i32::MAX as u32
    }

    pub fn matches_request(&self, request: &ActivityObservation) -> bool {
        self.session_id == request.key.session.id.parse().ok()
            && self.request_id == request.key.request.as_ref().and_then(|key| key.id.parse().ok())
            && self.session_id.is_some()
            && self.request_id.is_some()
    }

    pub fn matches_reported_holder(&self, waiter: &MssqlPageTaskWait) -> bool {
        waiter.blocking_task_address.as_deref() == Some(self.task_address.as_str())
            && waiter.blocking_session_id.and_then(|id| u32::try_from(id).ok()) == self.session_id
            && self.session_id.is_some()
            && waiter.blocking_execution_context_id.and_then(|id| u32::try_from(id).ok())
                == Some(self.execution_context_id)
    }

    /// A missing native field leaves task-only context. Contradictory native fields do not qualify a holder.
    pub fn is_consistent_with_reported_holder(&self, waiter: &MssqlPageTaskWait) -> bool {
        waiter.blocking_task_address.as_deref() == Some(self.task_address.as_str())
            && !waiter.blocking_session_id.is_some_and(|id| id < 0)
            && waiter
                .blocking_session_id
                .filter(|id| *id > 0)
                .is_none_or(|id| self.session_id.is_none_or(|session| u32::try_from(id).ok() == Some(session)))
            && waiter
                .blocking_execution_context_id
                .filter(|id| *id >= 0)
                .is_none_or(|id| u32::try_from(id).ok() == Some(self.execution_context_id))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPageTaskHolder {
    pub relation: PageTaskHolderRelation,
    pub matching_owner_rows: u32,
    /// Native task fields remain useful when the source cannot qualify a client request or its database.
    pub task: Option<MssqlPageTaskIdentity>,
    /// Same-point native request identity, with SQL attached only after exact generation/locator rechecks.
    pub request: Option<Box<ActivityObservation>>,
}

impl MssqlPageTaskHolder {
    pub fn is_valid(&self) -> bool {
        self.matching_owner_rows <= MAX_PAGE_TASK_HOLDER_ROWS
            && self.task.as_ref().is_none_or(MssqlPageTaskIdentity::is_valid)
            && self.request.as_ref().is_none_or(|row| {
                row.is_valid()
                    && row.key.request.is_some()
                    && row.scope == ActivityScopeBasis::RequestDatabase
                    && row.statement_identity.is_some()
                    && self.task.as_ref().is_some_and(|task| task.matches_request(row))
            })
            && match self.relation {
                PageTaskHolderRelation::QualifiedRequest => {
                    self.matching_owner_rows == 1 && self.task.is_some() && self.request.is_some()
                }
                PageTaskHolderRelation::TaskOnly | PageTaskHolderRelation::OtherDatabase => {
                    self.matching_owner_rows == 1 && self.task.is_some() && self.request.is_none()
                }
                PageTaskHolderRelation::Ambiguous => {
                    self.matching_owner_rows == MAX_PAGE_TASK_HOLDER_ROWS
                        && self.task.is_none()
                        && self.request.is_none()
                }
                PageTaskHolderRelation::Changed => self.matching_owner_rows <= 1 && self.request.is_none(),
                PageTaskHolderRelation::NotReported
                | PageTaskHolderRelation::Sentinel
                | PageTaskHolderRelation::SelfReference
                | PageTaskHolderRelation::Unavailable => {
                    self.matching_owner_rows == 0 && self.task.is_none() && self.request.is_none()
                }
            }
    }

    fn fits_task(&self, waiter: Option<&MssqlPageTaskWait>) -> bool {
        if !self.is_valid() {
            return false;
        }
        let Some(waiter) = waiter else { return self.relation == PageTaskHolderRelation::Unavailable };
        match self.relation {
            PageTaskHolderRelation::QualifiedRequest | PageTaskHolderRelation::OtherDatabase => self
                .task
                .as_ref()
                .is_some_and(|task| task.matches_reported_holder(waiter) && task.task_address != waiter.task_address),
            PageTaskHolderRelation::TaskOnly => self.task.as_ref().is_some_and(|task| {
                task.is_consistent_with_reported_holder(waiter) && task.task_address != waiter.task_address
            }),
            PageTaskHolderRelation::NotReported => {
                waiter.blocking_task_address.as_deref().is_none_or(|address| !nonzero_address(address))
            }
            PageTaskHolderRelation::Sentinel => waiter.blocking_session_id.is_some_and(|id| id < 0),
            PageTaskHolderRelation::SelfReference => {
                waiter.blocking_task_address.as_deref() == Some(waiter.task_address.as_str())
            }
            PageTaskHolderRelation::Changed
            | PageTaskHolderRelation::Ambiguous
            | PageTaskHolderRelation::Unavailable => true,
        }
    }

    fn redact(&mut self, engine: crate::Engine) {
        if let Some(request) = &mut self.request {
            request.redact(engine);
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPageTaskHolderPoint {
    pub observed_at: String,
    pub database_writable: Option<bool>,
    pub request: PageWaitRequestState,
    pub wait: PageWaitRelation,
    pub holder: MssqlPageTaskHolder,
}

impl MssqlPageTaskHolderPoint {
    fn fits_task(&self, task: Option<&MssqlPageTaskWait>) -> bool {
        canonical_time(&self.observed_at).is_ok()
            && task.is_none_or(|task| task.is_valid() && task.observed_at == self.observed_at)
            && self.holder.fits_task(task)
            && (self.request == PageWaitRequestState::SameStatement || self.wait == PageWaitRelation::Unqualified)
            && (self.holder.relation != PageTaskHolderRelation::QualifiedRequest
                || self.request == PageWaitRequestState::SameStatement
                    && self.wait == PageWaitRelation::OriginalObserved)
    }

    fn original_wait_not_observed(&self) -> bool {
        matches!(self.request, PageWaitRequestState::Missing | PageWaitRequestState::Changed)
            || self.request == PageWaitRequestState::SameStatement
                && matches!(self.wait, PageWaitRelation::NoLongerObserved | PageWaitRelation::Different)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PageTaskHolderTargetCheck {
    pub observed_at: String,
    pub target: Box<MappedPageTarget>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PageTaskHolderObservation {
    pub observed_from: String,
    pub observed_until: String,
    pub focus_key: String,
    pub target_state: PageWriterTargetState,
    /// One canonical new page/request/task observation. Its task is the first holder point's raw native row.
    pub page: Option<Box<PageWaitObservation>>,
    /// Independent final header/catalog point. It never overwrites the earlier page's checked mapping or clock.
    pub final_target: Option<Box<PageTaskHolderTargetCheck>>,
    pub before: MssqlPageTaskHolderPoint,
    /// The final task row is a distinct native observation, not a rewritten first point.
    pub after_task: Option<MssqlPageTaskWait>,
    pub after: MssqlPageTaskHolderPoint,
}

impl PageTaskHolderObservation {
    pub fn is_valid(&self) -> bool {
        let Some(bounds) = ObservationBounds::new(&self.observed_from, &self.observed_until) else { return false };
        let before_task = self.page.as_ref().and_then(|page| page.task_wait.as_ref());
        valid_digest(&self.focus_key)
            && bounds.contains(&self.before.observed_at)
            && bounds.contains(&self.after.observed_at)
            && ObservationBounds::new(&self.before.observed_at, &self.after.observed_at).is_some()
            && self.before.fits_task(before_task)
            && self.after.fits_task(self.after_task.as_ref())
            && self.page.as_ref().is_none_or(|page| {
                page.is_valid()
                    && page
                        .target
                        .as_ref()
                        .is_none_or(|target| matches!(target.scope, PageTargetScope::RequestDatabase))
                    && before_task.is_some()
                    && bounds.contains(&page.observed_from)
                    && bounds.contains(&page.observed_until)
            })
            && self.final_target.as_ref().is_none_or(|check| {
                bounds.contains(&check.observed_at)
                    && check.target.is_valid()
                    && matches!(check.target.scope, PageTargetScope::RequestDatabase)
                    && ObservationBounds::new(&check.observed_at, &self.after.observed_at).is_some()
                    && self.page.as_ref().is_some_and(|page| {
                        ObservationBounds::new(&page.observed_until, &check.observed_at).is_some()
                            && page.observed_address.is_some_and(|address| address.matches_header(&check.target.header))
                    })
            })
            && (self.target_state != PageWriterTargetState::NoLongerObserved || self.after.original_wait_not_observed())
            && (self.target_state != PageWriterTargetState::CurrentMapped
                || self.page.as_ref().is_some_and(|page| {
                    page.target.as_ref().is_some_and(|target| {
                        target.writer_target_key().is_ok_and(|key| {
                            self.final_target
                                .as_ref()
                                .is_some_and(|check| check.target.writer_target_key().ok() == Some(key))
                        })
                    })
                }))
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(&("page-task-holder-v1", &self.focus_key))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }

    pub fn redact(&mut self, engine: crate::Engine) {
        if let Some(page) = &mut self.page {
            page.redact(engine)
        }
        self.before.holder.redact(engine);
        self.after.holder.redact(engine);
    }
}

fn nonzero_address(value: &str) -> bool {
    value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) && value.bytes().any(|byte| byte != b'0')
}

#[cfg(test)]
mod tests;
