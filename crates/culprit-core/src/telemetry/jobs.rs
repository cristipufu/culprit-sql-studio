//! Timescale scheduler points retain native job state and bounded run records, never worker ownership.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

mod points;
pub use points::*;
mod continuous_aggregates;
pub use continuous_aggregates::*;
mod agent;
mod profile;
pub use agent::*;
pub use profile::JobsSnapshot;

pub const TIMESCALE_JOBS_SOURCE: &str = "timescale-jobs";
pub const TIMESCALE_JOB_RUNS_SOURCE: &str = "timescale-job-runs";
pub const TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE: &str = "timescale-continuous-aggregates";
pub const MAX_TIMESCALE_JOB_OBSERVATIONS: usize = 512;
pub const MAX_TIMESCALE_JOB_LABEL_BYTES: usize = 1024;
pub const TIMESCALE_JOB_HISTORY_HOURS: i64 = 24;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TimescaleJobViewKind {
    Jobs,
    Statistics,
    History,
    Errors,
    ContinuousAggregates,
}
impl TimescaleJobViewKind {
    pub fn native_name(self) -> &'static str {
        match self {
            Self::Jobs => "jobs",
            Self::Statistics => "job_stats",
            Self::History => "job_history",
            Self::Errors => "job_errors",
            Self::ContinuousAggregates => "continuous_aggregates",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TimescaleJobViewState {
    Available,
    Missing,
    PermissionDenied,
    UnsupportedColumns,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleJobViewCapability {
    pub kind: TimescaleJobViewKind,
    pub state: TimescaleJobViewState,
    /// OID and discovered column fingerprint belong to this extension/database capture, not a global incarnation.
    pub identity: Option<NativeIdentity>,
}
impl TimescaleJobViewCapability {
    fn is_valid(&self, extension: Option<&NativeIdentity>) -> bool {
        match (&self.identity, self.state) {
            (None, TimescaleJobViewState::Missing) => true,
            (Some(identity), state) if state != TimescaleJobViewState::Missing => {
                identity.is_valid()
                    && oid(&identity.id)
                    && identity.qualifiers.len() == 3
                    && identity.qualifiers.get("view").map(String::as_str) == Some(self.kind.native_name())
                    && identity.qualifiers.get("columns").is_some_and(|value| valid_digest(value))
                    && extension
                        .is_some_and(|extension| identity.qualifiers.get("extension_oid") == Some(&extension.id))
            }
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TimescaleJobRunVisibility {
    DatabaseOwner,
    JobOwners,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleJobRunWindow {
    pub from: String,
    pub until: String,
}
impl TimescaleJobRunWindow {
    pub fn contains(&self, at: &str) -> bool {
        ObservationBounds::new(&self.from, &self.until).is_some_and(|bounds| bounds.contains(at))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TimescaleJobsMetadata {
    /// Extension OID, database OID and installed version; none means the extension was not present.
    pub extension: Option<NativeIdentity>,
    pub views: Vec<TimescaleJobViewCapability>,
    pub run_view: Option<TimescaleJobViewKind>,
    pub run_visibility: TimescaleJobRunVisibility,
    pub successful_runs_logged: Option<bool>,
    /// The selected native start-time window is not a guaranteed retention period.
    pub run_window: TimescaleJobRunWindow,
    pub database_writable: Option<bool>,
}
impl TimescaleJobsMetadata {
    pub fn view(&self, kind: TimescaleJobViewKind) -> Option<&TimescaleJobViewCapability> {
        self.views.iter().find(|view| view.kind == kind)
    }
    pub fn available(&self, kind: TimescaleJobViewKind) -> bool {
        self.view(kind).is_some_and(|view| view.state == TimescaleJobViewState::Available)
    }
    pub fn is_valid(&self) -> bool {
        if self.views.len() != 5 || ObservationBounds::new(&self.run_window.from, &self.run_window.until).is_none() {
            return false;
        }
        let mut kinds = BTreeSet::new();
        if !self.views.iter().all(|view| kinds.insert(view.kind) && view.is_valid(self.extension.as_ref())) {
            return false;
        }
        match &self.extension {
            None => {
                self.views.iter().all(|view| view.state == TimescaleJobViewState::Missing)
                    && self.run_view.is_none()
                    && self.run_visibility == TimescaleJobRunVisibility::Unknown
                    && self.successful_runs_logged.is_none()
            }
            Some(extension) => {
                extension.is_valid()
                    && oid(&extension.id)
                    && extension.qualifiers.len() == 2
                    && extension.qualifiers.get("database_oid").is_some_and(|value| oid(value))
                    && extension.qualifiers.get("version").is_some_and(|value| label(value, 128))
                    && self.run_view.is_none_or(|kind| {
                        matches!(kind, TimescaleJobViewKind::History | TimescaleJobViewKind::Errors)
                            && self.available(kind)
                    })
            }
        }
    }
    pub fn accepts(&self, snapshot: &StateSnapshotDescription, row: &OperationalObservation) -> bool {
        if !row.is_valid() {
            return false;
        }
        self.accepts_between(&snapshot.observed_from, &snapshot.observed_until, row)
    }
    fn accepts_between(&self, from: &str, until: &str, row: &OperationalObservation) -> bool {
        let bounds = ObservationBounds::new(from, until);
        match &row.fact {
            OperationalFact::TimescaleJobState(point) => {
                row.source == TIMESCALE_JOBS_SOURCE
                    && self.available(TimescaleJobViewKind::Jobs)
                    && bounds.is_some_and(|bounds| bounds.contains(&point.observed_at))
                    && (point.statistics.is_none() || self.available(TimescaleJobViewKind::Statistics))
            }
            OperationalFact::TimescaleJobRun(point) => {
                row.source == TIMESCALE_JOB_RUNS_SOURCE
                    && bounds.is_some_and(|bounds| bounds.contains(&point.observed_at))
                    && self.run_window.contains(&point.started_at)
                    && point.procedure.as_ref().is_none_or(|procedure| {
                        procedure.origin == TimescaleJobProcedureOrigin::Unknown
                            || self
                                .run_view
                                .zip(self.extension.as_ref().and_then(|extension| extension.qualifiers.get("version")))
                                .is_some_and(|(view, version)| {
                                    procedure.origin == TimescaleJobProcedureOrigin::for_view(version, view)
                                })
                    })
                    && match self.run_view {
                        Some(TimescaleJobViewKind::History) => point.native_run_id.is_some(),
                        Some(TimescaleJobViewKind::Errors) => {
                            point.native_run_id.is_none() && point.succeeded.is_none()
                        }
                        _ => false,
                    }
            }
            OperationalFact::TimescaleContinuousAggregate(point) => {
                row.source == TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE
                    && self.available(TimescaleJobViewKind::ContinuousAggregates)
                    && bounds.is_some_and(|bounds| bounds.contains(&point.observed_at))
            }
            _ => false,
        }
    }
    pub fn matches_snapshot(&self, snapshot: &StateSnapshotDescription) -> bool {
        self.is_valid()
            && snapshot.is_valid()
            && snapshot.dataset == SnapshotDataset::Jobs
            && snapshot.records as usize <= MAX_TIMESCALE_JOB_OBSERVATIONS
            && snapshot.sources.len() == 3
            && snapshot.sources.iter().all(|source| {
                source.source != TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE
                    || source.records <= MAX_TIMESCALE_CONTINUOUS_AGGREGATE_ROWS
            })
            && snapshot.sources.iter().map(|source| source.source.as_str()).collect::<BTreeSet<_>>()
                == BTreeSet::from([
                    TIMESCALE_JOBS_SOURCE,
                    TIMESCALE_JOB_RUNS_SOURCE,
                    TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE,
                ])
            && canonical_time(&self.run_window.until).ok() == canonical_time(&snapshot.observed_from).ok()
    }
    pub fn matches_source(&self, source: &CollectionDescription, snapshot: &StateSnapshotDescription) -> bool {
        self.matches_snapshot(snapshot)
            && source.source == TIMESCALE_JOBS_SOURCE
            && source.target.engine() == crate::Engine::Postgres
            && source.target.database().is_some()
            && source.partition.is_valid_for(&source.target)
            && matches!(source.partition, SourcePartition::Target)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TimescaleJobsSnapshot {
    pub context: NativeReadContext,
    pub metadata: TimescaleJobsMetadata,
    pub sources: Vec<StateSourceCoverage>,
    pub observations: Vec<OperationalObservation>,
}
impl TimescaleJobsSnapshot {
    pub fn from_source(source: &CollectionDescription, observations: Vec<OperationalObservation>) -> Result<Self> {
        let SourceObservation::Jobs { snapshot, metadata } = &source.observation else {
            return Err(invalid());
        };
        if !metadata.matches_source(source, snapshot) {
            return Err(invalid());
        }
        let value = Self {
            context: NativeReadContext {
                target: source.target.clone(),
                partition: source.partition.clone(),
                observed_from: snapshot.observed_from.clone(),
                observed_until: snapshot.observed_until.clone(),
                database_writable: metadata.database_writable,
            },
            metadata: metadata.as_ref().clone(),
            sources: snapshot.sources.clone(),
            observations,
        };
        value.is_valid_read(false).then_some(value).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        self.is_valid_read(true)
    }
    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        if self.sources.len() != 3
            || self.sources.iter().any(|source| source.records as usize > MAX_TIMESCALE_JOB_OBSERVATIONS)
        {
            return false;
        }
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::Jobs,
            observed_from: self.context.observed_from.clone(),
            observed_until: self.context.observed_until.clone(),
            records: self.sources.iter().map(|source| source.records).sum(),
            sources: self.sources.clone(),
        };
        let source = CollectionDescription {
            target: self.context.target.clone(),
            partition: self.context.partition.clone(),
            source: TIMESCALE_JOBS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Jobs {
                snapshot: snapshot.clone(),
                metadata: Box::new(self.metadata.clone()),
            },
        };
        if !self.context.is_valid()
            || self.context.database_writable != self.metadata.database_writable
            || !self.metadata.matches_source(&source, &snapshot)
            || self.observations.len() > MAX_TIMESCALE_JOB_OBSERVATIONS
            || u64::from(snapshot.records) > MAX_TIMESCALE_JOB_OBSERVATIONS as u64
        {
            return false;
        }
        let mut counts: BTreeMap<_, _> =
            self.sources.iter().map(|source| (source.source.as_str(), (source, 0usize))).collect();
        let mut keys = BTreeSet::new();
        for row in &self.observations {
            let Some((coverage, count)) = counts.get_mut(row.source.as_str()) else {
                return false;
            };
            if !row.is_valid()
                || !self.accepts(row)
                || !keys.insert((&row.source, &row.key))
                || !matches!(coverage.state, crate::SourceState::Collected | crate::SourceState::Partial)
            {
                return false;
            }
            *count += 1;
        }
        counts.values().all(|(source, count)| {
            *count <= source.records as usize && (!read_complete || *count == source.records as usize)
        })
    }
    pub fn accepts(&self, row: &OperationalObservation) -> bool {
        row.is_valid() && self.metadata.accepts_between(&self.context.observed_from, &self.context.observed_until, row)
    }
}

fn oid(value: &str) -> bool {
    counter(value) && value.parse::<u32>().is_ok_and(|value| value > 0)
}
fn counter(value: &str) -> bool {
    value.parse::<u64>().is_ok_and(|number| number.to_string() == value)
}
fn label(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.contains('\0')
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
