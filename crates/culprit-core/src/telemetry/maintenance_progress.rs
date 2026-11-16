//! Native index-build progress is a bounded point, not completion history, lock acquisition or resource ownership.
use super::*;

pub const MAX_MAINTENANCE_PROGRESS_OBSERVATIONS: usize = 1;
pub const MAX_MAINTENANCE_PROGRESS_PHASE_BYTES: usize = 512;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum IndexBuildCommand {
    CreateIndex,
    CreateIndexConcurrently,
    Reindex,
    ReindexConcurrently,
}

impl IndexBuildCommand {
    /// These are native catalog values, not localized display text or a classifier of arbitrary SQL.
    pub fn from_native(value: &str) -> Option<Self> {
        match value {
            "CREATE INDEX" => Some(Self::CreateIndex),
            "CREATE INDEX CONCURRENTLY" => Some(Self::CreateIndexConcurrently),
            "REINDEX" => Some(Self::Reindex),
            "REINDEX CONCURRENTLY" => Some(Self::ReindexConcurrently),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresIndexBuildProgress {
    /// Native counts concern the current phase. They are neither allocated bytes nor whole-command completion.
    pub blocks_total: Option<String>,
    pub blocks_done: Option<String>,
    pub tuples_total: Option<String>,
    pub tuples_done: Option<String>,
    pub lockers_total: Option<String>,
    pub lockers_done: Option<String>,
    /// A raw PID with no generation cannot authorize a session/SQL join. Native zero remains zero.
    pub current_locker_pid: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct YsqlIndexBuildProgress {
    pub tuples_done: Option<String>,
    /// DocDB's native backfill estimate is independent from PostgreSQL heap-block work and measured completion.
    pub tuples_total_estimate: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum IndexBuildProgressWork {
    PostgreSql { progress: Box<PostgresIndexBuildProgress> },
    Ysql { progress: Box<YsqlIndexBuildProgress> },
}

impl IndexBuildProgressWork {
    pub fn engine(&self) -> crate::Engine {
        match self {
            Self::PostgreSql { .. } => crate::Engine::Postgres,
            Self::Ysql { .. } => crate::Engine::Yugabyte,
        }
    }

    pub fn is_valid(&self) -> bool {
        let values = match self {
            Self::PostgreSql { progress } => vec![
                &progress.blocks_total,
                &progress.blocks_done,
                &progress.tuples_total,
                &progress.tuples_done,
                &progress.lockers_total,
                &progress.lockers_done,
                &progress.current_locker_pid,
            ],
            Self::Ysql { progress } => vec![&progress.tuples_done, &progress.tuples_total_estimate],
        };
        values.into_iter().flatten().all(|value| canonical_counter(value))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MaintenanceProgressObservation {
    pub observed_at: String,
    pub actor: ActivityKey,
    pub statement: Option<NativeIdentity>,
    /// OIDs are native references inside this read's database/member context, not object incarnation proofs.
    pub database_oid: String,
    pub relation_oid: String,
    /// Native zero is retained, notably for a non-concurrent PostgreSQL CREATE INDEX.
    pub index_oid: String,
    pub command: IndexBuildCommand,
    pub phase: String,
    pub work: IndexBuildProgressWork,
}

impl MaintenanceProgressObservation {
    pub fn fits_source_scope(&self, source: &CollectionDescription) -> bool {
        let SourceObservation::MaintenanceProgress { snapshot, result } = &source.observation else { return false };
        result.is_valid_source(source, snapshot) && self.matches_result(&result.read_context(source, snapshot), result)
    }

    fn matches_result(&self, context: &NativeReadContext, result: &MaintenanceProgressResult) -> bool {
        self.is_valid()
            && self.work.engine() == context.target.engine()
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
            && result.membership == MaintenanceProgressMembership::Observed
            && result.focus.as_ref().is_none_or(|focus| {
                self.actor == focus.original.key
                    && self.statement.as_ref() == Some(&focus.statement_identity)
                    && ObservationBounds::new(&focus.original_observed_at, &self.observed_at).is_some()
            })
    }

    pub fn is_valid(&self) -> bool {
        valid_time(&self.observed_at)
            && self.actor.is_valid()
            && self.actor.request.is_some()
            && self.statement.as_ref().is_none_or(NativeIdentity::is_valid)
            && oid(&self.database_oid)
            && self.database_oid != "0"
            && oid(&self.relation_oid)
            && oid(&self.index_oid)
            && !self.phase.is_empty()
            && self.phase.len() <= MAX_MAINTENANCE_PROGRESS_PHASE_BYTES
            && !self.phase.chars().any(char::is_control)
            && self.work.is_valid()
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(&(&self.actor, &self.database_oid, &self.relation_oid, &self.index_oid))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }

    pub fn relation_reference(&self) -> Option<NativeIdentity> {
        (self.relation_oid != "0")
            .then(|| NativeIdentity::new(&self.relation_oid).qualified("database_oid", &self.database_oid))
    }

    pub fn index_reference(&self) -> Option<NativeIdentity> {
        (self.index_oid != "0")
            .then(|| NativeIdentity::new(&self.index_oid).qualified("database_oid", &self.database_oid))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MaintenanceProgressBinding {
    Independent,
    Matched,
    UnverifiedOriginalScope,
    ParticipantMissing,
    ParticipantChanged,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MaintenanceProgressMembership {
    Observed,
    NotObserved,
    Unavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MaintenanceProgressResult {
    /// Retained once in the source descriptor. It is absent for an independent live point.
    pub focus: Option<Box<ActivityRequestFocus>>,
    /// Native role observed by this read, not the original role, login permissions or configured routing intent.
    pub database_writable: Option<bool>,
    pub binding: MaintenanceProgressBinding,
    pub membership: MaintenanceProgressMembership,
}

impl MaintenanceProgressResult {
    pub fn is_valid_header(&self, snapshot: &StateSnapshotDescription) -> bool {
        snapshot.is_valid()
            && snapshot.dataset == SnapshotDataset::MaintenanceProgress
            && snapshot.sources.len() == 1
            && snapshot.records as usize <= MAX_MAINTENANCE_PROGRESS_OBSERVATIONS
            && match &self.focus {
                None => self.binding == MaintenanceProgressBinding::Independent,
                Some(focus) => {
                    self.binding != MaintenanceProgressBinding::Independent
                        && focus.is_valid()
                        && ObservationBounds::new(&focus.original_observed_at, &snapshot.observed_until).is_some()
                }
            }
            && match self.membership {
                MaintenanceProgressMembership::Observed => {
                    snapshot.records == 1
                        && !matches!(
                            self.binding,
                            MaintenanceProgressBinding::ParticipantMissing
                                | MaintenanceProgressBinding::ParticipantChanged
                        )
                }
                MaintenanceProgressMembership::NotObserved => snapshot.records == 0,
                MaintenanceProgressMembership::Unavailable => {
                    snapshot.records == 0
                        && matches!(
                            snapshot.sources[0].state,
                            crate::SourceState::Partial
                                | crate::SourceState::Unavailable
                                | crate::SourceState::NotApplicable
                        )
                }
            }
    }

    /// Partition consistency delegates here; do not recurse through NativeReadContext::from_snapshot.
    pub fn is_valid_source(&self, source: &CollectionDescription, snapshot: &StateSnapshotDescription) -> bool {
        self.is_valid_header(snapshot)
            && self.is_valid_for(&self.read_context(source, snapshot))
            && snapshot
                .sources
                .iter()
                .all(|coverage| coverage_fits_context(&self.read_context(source, snapshot), coverage))
    }

    pub fn read_context(
        &self,
        source: &CollectionDescription,
        snapshot: &StateSnapshotDescription,
    ) -> NativeReadContext {
        NativeReadContext {
            target: source.target.clone(),
            partition: source.partition.clone(),
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
            database_writable: self.database_writable,
        }
    }

    pub fn is_valid_for(&self, context: &NativeReadContext) -> bool {
        if !context.is_valid()
            || context.target.database().is_none()
            || self.database_writable != context.database_writable
        {
            return false;
        }
        match &self.focus {
            None => self.binding == MaintenanceProgressBinding::Independent,
            Some(focus) => {
                focus.is_valid()
                    && focus.target.telemetry_id().ok() == context.target.telemetry_id().ok()
                    && !matches!((focus.database_writable, context.database_writable), (Some(before), Some(after)) if before != after)
                    && ObservationBounds::new(&focus.original_observed_at, &context.observed_until).is_some()
                    && match self.binding {
                        MaintenanceProgressBinding::Independent => false,
                        MaintenanceProgressBinding::UnverifiedOriginalScope => {
                            !context.has_qualified_local_owner() || focus.partition == SourcePartition::Target
                        }
                        _ => original_scope_matches(focus, context),
                    }
            }
        }
    }

    pub fn original_reference(&self) -> Option<&DiagnosticEvidenceRef> {
        self.focus.as_ref().map(|focus| &focus.parent)
    }

    pub fn expected_actor(&self) -> Option<&ActivityKey> {
        self.focus.as_ref().map(|focus| &focus.original.key)
    }
}

/// The current read's native owner never fills a missing original physical member.
pub fn maintenance_progress_original_scope_matches(focus: &ActivityRequestFocus, context: &NativeReadContext) -> bool {
    focus.is_valid() && original_scope_matches(focus, context)
}

fn original_scope_matches(focus: &ActivityRequestFocus, context: &NativeReadContext) -> bool {
    context.has_qualified_local_owner()
        && matches!(&focus.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) })
        && focus.target.telemetry_id().ok() == context.target.telemetry_id().ok()
        && focus.partition == context.partition
        && focus.database_writable == context.database_writable
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MaintenanceProgressSnapshot {
    pub source: String,
    pub context: NativeReadContext,
    pub coverage: StateSourceCoverage,
    pub result: MaintenanceProgressResult,
    pub observations: Vec<MaintenanceProgressObservation>,
}

impl MaintenanceProgressSnapshot {
    pub fn is_valid(&self) -> bool {
        self.is_valid_read(true)
    }

    /// Readback gaps never rewrite the original source count or membership verdict.
    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        let header = StateSnapshotDescription {
            dataset: SnapshotDataset::MaintenanceProgress,
            observed_from: self.context.observed_from.clone(),
            observed_until: self.context.observed_until.clone(),
            records: self.coverage.records,
            sources: vec![self.coverage.clone()],
        };
        valid_key(&self.source)
            && self.result.is_valid_header(&header)
            && self.result.is_valid_for(&self.context)
            && self.coverage.source == self.source
            && self.observations.len() <= self.coverage.records as usize
            && (!read_complete || self.coverage.records as usize == self.observations.len())
            && self.observations.len() <= MAX_MAINTENANCE_PROGRESS_OBSERVATIONS
            && coverage_fits_context(&self.context, &self.coverage)
            && self.observations.iter().all(|row| row.matches_result(&self.context, &self.result))
    }
}

fn coverage_fits_context(context: &NativeReadContext, coverage: &StateSourceCoverage) -> bool {
    context.has_qualified_local_owner() || !coverage.complete_scope
}

fn valid_time(value: &str) -> bool {
    value.len() <= 64 && canonical_time(value).is_ok()
}

fn canonical_counter(value: &str) -> bool {
    valid_counter(value) && value.parse::<u64>().is_ok_and(|number| number.to_string() == value)
}

fn oid(value: &str) -> bool {
    canonical_counter(value) && value.parse::<u32>().is_ok()
}

#[cfg(test)]
mod tests;
