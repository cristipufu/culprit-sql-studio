//! One native request's work retains event identity, scope and stage parentage, not spill time or a root cause.
use super::*;
use crate::Engine;

mod native;
mod postgres;
pub use native::{CurrentRequestWorkNative, CurrentRequestWorkProfile};
pub use postgres::{PostgresCopyCommand, PostgresCopyIo, PostgresCopyProgress};

pub const MAX_CURRENT_REQUEST_WORK_SQL_BYTES: usize = 16 * 1024;
const MAX_NATIVE_STAGE_NAME_BYTES: usize = 512;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MysqlStageUnavailable {
    InstrumentationDisabled,
    PermissionDenied,
    Unsupported,
    NotCollected,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlStageEvent {
    pub thread_id: String,
    pub event_id: String,
    pub nesting_event_id: String,
    pub nesting_event_type: String,
    pub event_name: String,
    /// Native picosecond timers have a native origin and may be unavailable for an untimed stage.
    pub timer_start_ps: Option<String>,
    pub timer_wait_ps: Option<String>,
    /// Native stage work units, not bytes or a fraction of the whole statement's completion.
    pub work_completed: Option<String>,
    pub work_estimated: Option<String>,
}

impl MysqlStageEvent {
    fn belongs_to(&self, request: &MysqlCurrentRequestWork) -> bool {
        self.thread_id == request.thread_id
            && self.nesting_event_id == request.statement_event_id
            && self.nesting_event_type == "STATEMENT"
            && positive_id(&self.event_id)
            && self.event_id != self.nesting_event_id
            && !self.event_name.is_empty()
            && self.event_name.len() <= MAX_NATIVE_STAGE_NAME_BYTES
            && !self.event_name.chars().any(char::is_control)
            && [&self.timer_start_ps, &self.timer_wait_ps, &self.work_completed, &self.work_estimated]
                .into_iter()
                .flatten()
                .all(|value| exact_counter(value))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum MysqlStageObservation {
    Observed {
        event: Box<MysqlStageEvent>,
    },
    NotObserved,
    Unavailable {
        reason: MysqlStageUnavailable,
    },
    /// A current thread's stage did not establish parentage to the selected statement event.
    Unlinked,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlCurrentRequestWork {
    pub thread_id: String,
    pub statement_event_id: String,
    pub timer_start_ps: String,
    pub timer_wait_ps: Option<String>,
    pub rows_examined: Option<String>,
    pub rows_sent: Option<String>,
    pub created_tmp_tables: Option<String>,
    pub created_tmp_disk_tables: Option<String>,
    pub sort_merge_passes: Option<String>,
    pub stage: MysqlStageObservation,
}

impl MysqlCurrentRequestWork {
    pub fn is_valid(&self) -> bool {
        positive_id(&self.thread_id)
            && positive_id(&self.statement_event_id)
            && exact_counter(&self.timer_start_ps)
            && [
                &self.timer_wait_ps,
                &self.rows_examined,
                &self.rows_sent,
                &self.created_tmp_tables,
                &self.created_tmp_disk_tables,
                &self.sort_merge_passes,
            ]
            .into_iter()
            .flatten()
            .all(|value| exact_counter(value))
            && match &self.stage {
                MysqlStageObservation::Observed { event } => event.belongs_to(self),
                _ => true,
            }
    }

    fn matches_actor(&self, actor: &ActivityKey) -> bool {
        actor.session.generation == self.thread_id
            && actor.request.as_ref().is_some_and(|request| {
                request.id == self.statement_event_id && request.generation == self.timer_start_ps
            })
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CurrentRequestWorkObservation {
    pub observed_at: String,
    pub actor: ActivityKey,
    pub database: String,
    /// Request schema and session default schema are different source facts.
    pub scope: ActivityScopeBasis,
    pub statement_identity: NativeIdentity,
    pub statement: DefinitionPayload,
    pub native: CurrentRequestWorkNative,
}

impl CurrentRequestWorkObservation {
    pub fn is_valid(&self) -> bool {
        self.observed_at.len() <= 64
            && canonical_time(&self.observed_at).is_ok()
            && self.actor.is_valid()
            && !self.database.is_empty()
            && self.database.len() <= 1024
            && !self.database.chars().any(char::is_control)
            && matches!(self.scope, ActivityScopeBasis::RequestDatabase | ActivityScopeBasis::SessionDatabase)
            && self.native.is_valid()
            && self.native.matches_embedded_actor(&self.actor)
            && self.native.profile().matches_identity(&self.actor, self.scope, &self.statement_identity)
            && self.statement.is_valid()
            && match &self.statement {
                DefinitionPayload::Available { text, .. } => text.len() <= MAX_CURRENT_REQUEST_WORK_SQL_BYTES,
                DefinitionPayload::Unavailable { .. } => true,
            }
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(&(&self.actor, &self.statement_identity, &self.database, self.scope))
            .map(|value| digest(&value))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }

    pub fn fits_source_scope(&self, source: &CollectionDescription) -> bool {
        let SourceObservation::CurrentRequestWork { snapshot, result, profile } = &source.observation else {
            return false;
        };
        self.native.profile() == *profile
            && result.is_valid_source(source, snapshot)
            && self.matches_result(&result.read_context(source, snapshot), result)
    }

    fn matches_result(&self, context: &NativeReadContext, result: &FocusedRequestResult) -> bool {
        self.is_valid()
            && context.target.engine() == self.native.engine()
            && context.target.database().is_some_and(|database| database.database == self.database)
            && result.matches_point(context, &self.actor, Some(&self.statement_identity), &self.observed_at)
    }

    pub fn redact(&mut self) {
        if let DefinitionPayload::Available { text, .. } = &self.statement {
            self.statement = DefinitionPayload::sql(text, self.native.engine());
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CurrentRequestWorkSnapshot {
    pub profile: CurrentRequestWorkProfile,
    pub source: String,
    pub context: NativeReadContext,
    pub coverage: StateSourceCoverage,
    pub result: FocusedRequestResult,
    pub observations: Vec<CurrentRequestWorkObservation>,
}

impl CurrentRequestWorkSnapshot {
    pub fn is_valid(&self) -> bool {
        self.is_valid_read(true)
    }

    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        let header = StateSnapshotDescription {
            dataset: SnapshotDataset::CurrentRequestWork,
            observed_from: self.context.observed_from.clone(),
            observed_until: self.context.observed_until.clone(),
            records: self.coverage.records,
            sources: vec![self.coverage.clone()],
        };
        valid_key(&self.source)
            && self.context.target.engine() == self.profile.engine()
            && self.result.focus.as_deref().is_none_or(|focus| self.profile.accepts_focus(focus))
            && self.result.is_valid_header(&header)
            && self.result.is_valid_for(&self.context)
            && self.coverage.source == self.source
            && self.observations.len() <= self.coverage.records as usize
            && (!read_complete || self.coverage.records as usize == self.observations.len())
            && self.observations.len() <= MAX_FOCUSED_REQUEST_OBSERVATIONS
            && focused_request_coverage_fits_context(&self.context, &self.coverage)
            && self
                .observations
                .iter()
                .all(|row| row.native.profile() == self.profile && row.matches_result(&self.context, &self.result))
    }
}

fn exact_counter(value: &str) -> bool {
    valid_counter(value) && value.parse::<u64>().is_ok_and(|number| number.to_string() == value)
}

fn positive_id(value: &str) -> bool {
    exact_counter(value) && value != "0"
}

#[cfg(test)]
mod tests;
