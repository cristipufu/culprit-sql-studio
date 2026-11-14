//! Capture-local task allocation/deallocation observations. Native request counters may include reservations,
//! cached objects and earlier statements in the same batch. They are not footprint, spill time or allocator proof.
use super::*;
use std::collections::BTreeSet;

pub const MAX_TASK_SPACE_OBSERVATIONS: usize = 4096;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[ts(export)]
pub struct TaskSpaceIdentity {
    pub session_id: String,
    pub request_id: String,
    pub execution_context_id: String,
}

impl TaskSpaceIdentity {
    pub fn is_valid(&self) -> bool {
        canonical_id(&self.session_id)
            && self.session_id != "0"
            && canonical_id(&self.request_id)
            && canonical_id(&self.execution_context_id)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TaskRequestState {
    SameStatement,
    Missing,
    Changed,
    Unverified,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct TaskRequestCheck {
    pub observed_at: String,
    pub state: TaskRequestState,
}

impl TaskRequestCheck {
    pub fn is_valid(&self) -> bool {
        canonical_time(&self.observed_at).is_ok()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TaskRequestContext {
    NativeJoinOnly,
    CheckedAtBounds,
    Changed,
    Missing,
    Unverified,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct TaskSpaceObservation {
    pub source: String,
    pub observed_at: String,
    /// Only a tuple within this capture. It has no task-generation evidence and cannot authorize a later delta.
    pub task: TaskSpaceIdentity,
    /// Native temporary database identifier, not ownership of the selected application database or Azure pool.
    pub native_database_id: String,
    pub bytes_per_page: u32,
    pub user_allocated_pages: Option<String>,
    pub user_deallocated_pages: Option<String>,
    pub internal_allocated_pages: Option<String>,
    pub internal_deallocated_pages: Option<String>,
    /// Native joined request context remains useful even when later checks cannot establish continuity.
    pub request: Option<ActivityObservation>,
    pub before_check: Option<TaskRequestCheck>,
    pub after_check: Option<TaskRequestCheck>,
    pub statement_identity: Option<NativeIdentity>,
    pub plan_identity: Option<NativeIdentity>,
}

impl TaskSpaceObservation {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&self.task).map(|value| digest(&value)).map_err(|_| CollectionStop::InvalidPage.error())
    }

    /// Checked endpoints qualify the request/statement context only. They do not supply a task incarnation or
    /// establish which statement in a multi-statement request produced the allocation counters.
    pub fn request_context(&self) -> TaskRequestContext {
        let states = [&self.before_check, &self.after_check];
        if states.into_iter().flatten().any(|check| check.state == TaskRequestState::Changed) {
            return TaskRequestContext::Changed;
        }
        if states.into_iter().flatten().any(|check| check.state == TaskRequestState::Missing) {
            return TaskRequestContext::Missing;
        }
        if self.request.is_none() {
            return TaskRequestContext::Unverified;
        }
        if states.iter().all(|check| check.is_none()) {
            return TaskRequestContext::NativeJoinOnly;
        }
        if states
            .into_iter()
            .all(|check| check.as_ref().is_some_and(|check| check.state == TaskRequestState::SameStatement))
        {
            TaskRequestContext::CheckedAtBounds
        } else {
            TaskRequestContext::Unverified
        }
    }

    pub fn fits_database_capture(&self, database: &str) -> bool {
        self.request.as_ref().is_none_or(|request| request.database_context_is_consistent(database))
    }

    pub fn is_valid(&self) -> bool {
        let checked = self.request_context() == TaskRequestContext::CheckedAtBounds;
        valid_key(&self.source)
            && canonical_time(&self.observed_at).is_ok()
            && self.task.is_valid()
            && canonical_id(&self.native_database_id)
            && self.native_database_id != "0"
            && self.bytes_per_page.is_power_of_two()
            && (512..=1_048_576).contains(&self.bytes_per_page)
            && [
                &self.user_allocated_pages,
                &self.user_deallocated_pages,
                &self.internal_allocated_pages,
                &self.internal_deallocated_pages,
            ]
            .into_iter()
            .flatten()
            .all(|value| valid_counter(value))
            && [&self.before_check, &self.after_check].into_iter().flatten().all(TaskRequestCheck::is_valid)
            && (self.request.is_some()
                || [&self.before_check, &self.after_check]
                    .into_iter()
                    .flatten()
                    .all(|check| check.state != TaskRequestState::SameStatement))
            && match (&self.before_check, &self.after_check) {
                (Some(before), Some(after)) => {
                    ObservationBounds::new(&before.observed_at, &after.observed_at).is_some()
                }
                _ => true,
            }
            && self.request.as_ref().is_none_or(|request| {
                request.is_valid()
                    && request.key.session.id == self.task.session_id
                    && request.key.request.as_ref().is_some_and(|key| key.id == self.task.request_id)
                    && request.scope == ActivityScopeBasis::RequestDatabase
                    && (checked || !request.statement.is_available())
            })
            && self.statement_identity.as_ref().is_none_or(|identity| {
                checked
                    && identity.is_valid()
                    && self.request.as_ref().is_some_and(|request| request.statement.is_available())
            })
            && self
                .plan_identity
                .as_ref()
                .is_none_or(|identity| identity.is_valid() && self.statement_identity.is_some())
    }

    pub fn redact(&mut self, engine: crate::Engine) {
        if let Some(request) = &mut self.request {
            request.redact(engine);
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct TaskSpaceSnapshot {
    pub source: String,
    pub observed_from: String,
    pub observed_until: String,
    pub observations: Vec<TaskSpaceObservation>,
}

impl TaskSpaceSnapshot {
    pub fn is_valid(&self) -> bool {
        let Some(bounds) = ObservationBounds::new(&self.observed_from, &self.observed_until) else { return false };
        let mut keys = BTreeSet::new();
        valid_key(&self.source)
            && self.observations.len() <= MAX_TASK_SPACE_OBSERVATIONS
            && self.observations.iter().all(|row| {
                row.is_valid()
                    && row.source == self.source
                    && bounds.contains(&row.observed_at)
                    && [&row.before_check, &row.after_check]
                        .into_iter()
                        .flatten()
                        .all(|check| bounds.contains(&check.observed_at))
                    && row.key().is_ok_and(|key| keys.insert(key))
            })
    }
}

fn canonical_id(value: &str) -> bool {
    valid_counter(value) && (value == "0" || !value.starts_with('0'))
}

#[cfg(test)]
mod tests;
