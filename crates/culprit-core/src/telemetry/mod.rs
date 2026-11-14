//! Bounded source observations. Export completeness describes reads, never atomicity or proven source continuity.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DatabaseIdentity, Error, ErrorCode, Result, t};

mod activity;
mod admission;
mod cached_definitions;
mod cached_statements;
mod catalog;
mod check_constraints;
mod columns;
mod compilation;
mod counters;
mod definitions;
mod delivery;
mod diagnostic_focus;
mod diagnostic_observation;
mod diagnostics;
mod discovery;
mod events;
mod fleet;
mod focus;
mod foreign_keys;
mod grants;
mod identity;
mod index_operations;
mod indexes;
mod io;
mod memory_failures;
mod missing_indexes;
mod monitor;
mod native_failures;
mod native_identity;
mod objects;
mod observation;
mod outbox;
mod page_waits;
mod partitions;
mod payloads;
mod persistent_versions;
mod queues;
mod read_accounting;
mod read_metrics;
mod recipes;
mod replication;
mod request_resources;
mod resources;
mod retained_plans;
mod retention;
mod runtime;
mod runtime_distributions;
mod session_memory;
mod settings;
mod state;
mod statement_counters;
mod statistics;
mod subject;
mod tables;
mod target;
mod task_space;
mod temporary_work;
mod transactions;
mod triggers;
mod tuning;
mod wait_semantics;
pub use activity::*;
pub use admission::*;
pub use cached_definitions::*;
pub use cached_statements::*;
pub use catalog::*;
pub use check_constraints::*;
pub use columns::*;
pub use compilation::*;
pub use counters::*;
pub use definitions::*;
pub use delivery::*;
pub use diagnostic_focus::*;
pub use diagnostic_observation::*;
pub use diagnostics::*;
pub use discovery::*;
pub use events::*;
pub use fleet::*;
pub use focus::*;
pub use foreign_keys::*;
pub use grants::*;
pub use identity::*;
pub use index_operations::*;
pub use indexes::*;
pub use io::*;
pub use memory_failures::*;
pub use missing_indexes::*;
pub use monitor::*;
pub use native_failures::*;
pub use native_identity::*;
pub use objects::*;
pub use observation::*;
pub use outbox::*;
pub use page_waits::*;
pub use partitions::*;
pub use payloads::*;
pub use persistent_versions::*;
pub use queues::*;
pub use read_accounting::*;
pub use read_metrics::*;
pub use recipes::*;
pub use replication::*;
pub use request_resources::*;
pub use resources::*;
pub use retained_plans::*;
pub use retention::*;
pub use runtime::*;
pub use runtime_distributions::*;
pub use session_memory::*;
pub use settings::*;
pub use state::*;
pub use statement_counters::*;
pub use statistics::*;
pub use subject::*;
pub use tables::*;
pub use target::*;
pub use task_space::*;
pub use temporary_work::*;
pub use transactions::*;
pub use triggers::*;
pub use tuning::*;
pub use wait_semantics::*;

pub const MAX_CURSOR_BYTES: usize = 16 * 1024;

fn valid_counter(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}
fn valid_key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}
fn valid_reset(reset: &crate::ResetMarker) -> bool {
    match reset {
        crate::ResetMarker::Unknown => true,
        crate::ResetMarker::At(at) => at.len() <= 64 && DateTime::parse_from_rfc3339(at).is_ok(),
        crate::ResetMarker::Generation(id) => valid_key(id),
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CollectionBudget {
    pub page_rows: u32,
    pub max_snapshot_rows: u32,
    pub page_bytes: u32,
    pub total_bytes: u32,
    pub max_sql_bytes: u32,
    pub max_plan_bytes: u32,
    pub max_pages: u32,
    pub max_queries: u32,
    pub statement_timeout_ms: u32,
    pub total_timeout_ms: u32,
}

impl Default for CollectionBudget {
    fn default() -> Self {
        Self {
            page_rows: 500,
            max_snapshot_rows: 500,
            page_bytes: 4 * 1024 * 1024,
            total_bytes: 64 * 1024 * 1024,
            max_sql_bytes: 256 * 1024,
            max_plan_bytes: 2 * 1024 * 1024,
            max_pages: 32,
            max_queries: 128,
            statement_timeout_ms: 5_000,
            total_timeout_ms: 60_000,
        }
    }
}

impl CollectionBudget {
    /// The validated protocol ceiling, not a suggested collection profile or a production cost qualification.
    /// Provider-owned limits clamp this envelope; an actual read still intersects its caller's budget.
    pub const fn upper_limits() -> Self {
        Self {
            page_rows: 10_000,
            max_snapshot_rows: 100_000,
            page_bytes: 8 * 1024 * 1024,
            total_bytes: 1024 * 1024 * 1024,
            max_sql_bytes: 4 * 1024 * 1024,
            max_plan_bytes: 4 * 1024 * 1024,
            max_pages: 10_000,
            max_queries: 100_000,
            statement_timeout_ms: 60_000,
            total_timeout_ms: 900_000,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let upper = Self::upper_limits();
        if !(1..=upper.max_snapshot_rows).contains(&self.max_snapshot_rows)
            || !(1..=upper.page_rows).contains(&self.page_rows)
            || !(1024..=upper.page_bytes).contains(&self.page_bytes)
            || self.total_bytes < self.page_bytes
            || self.total_bytes > upper.total_bytes
            || !(1024..=upper.max_sql_bytes).contains(&self.max_sql_bytes)
            || !(1024..=upper.max_plan_bytes).contains(&self.max_plan_bytes)
            || !(1..=upper.max_pages).contains(&self.max_pages)
            || !(1..=upper.max_queries).contains(&self.max_queries)
            || !(1..=upper.statement_timeout_ms).contains(&self.statement_timeout_ms)
            || self.total_timeout_ms < self.statement_timeout_ms
            || self.total_timeout_ms > upper.total_timeout_ms
        {
            return Err(Error::invalid(t!("collection.invalid_budget")));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CollectionRequest {
    pub subject: CollectionSubject,
    pub selection: CollectionSelection,
    pub budget: CollectionBudget,
}

impl CollectionRequest {
    pub fn collection(&self) -> TelemetryCollection {
        self.selection.collection()
    }
    pub fn bounds(&self) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
        let (from, until) = match &self.selection {
            CollectionSelection::QueryHistory { from, until }
            | CollectionSelection::ResourceHistory { from, until }
            | CollectionSelection::DeadlockHistory { from, until } => (from, until),
            _ => return Err(Error::invalid(t!("collection.invalid_interval"))),
        };
        history_bounds(from, until)
    }

    pub fn validate(&self) -> Result<()> {
        self.subject.validate()?;
        self.budget.validate()?;
        match &self.selection {
            CollectionSelection::FocusedPageWait { focus } => {
                self.subject.require_database()?;
                if !focus.is_valid() {
                    return Err(CollectionStop::InvalidPage.error());
                }
            }
            CollectionSelection::Diagnostics { parent, controller_version, step_id } => {
                if !parent.is_valid() || *controller_version == 0 || !valid_digest(step_id) {
                    return Err(CollectionStop::InvalidPage.error());
                }
            }
            CollectionSelection::Activity | CollectionSelection::Snapshot { .. } => {}
            _ => {
                self.bounds()?;
            }
        }
        Ok(())
    }
}

pub const MAX_HISTORY_WINDOW_SECONDS: u32 = 86_400;
fn history_bounds(from: &str, until: &str) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    bounded_history_bounds(from, until, MAX_HISTORY_WINDOW_SECONDS, || {
        Error::invalid(t!("collection.invalid_interval"))
    })
}
fn bounded_history_bounds(
    from: &str,
    until: &str,
    maximum_seconds: u32,
    invalid: fn() -> Error,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let parse = |value: &str| {
        if value.len() > 64 {
            return Err(invalid());
        }
        DateTime::parse_from_rfc3339(value).map(|time| time.with_timezone(&Utc)).map_err(|_| invalid())
    };
    let (from, until) = (parse(from)?, parse(until)?);
    if from >= until || until - from > chrono::Duration::seconds(i64::from(maximum_seconds)) {
        return Err(invalid());
    }
    Ok((from, until))
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TelemetryStream {
    QueryRuntime,
    QueryWaits,
    QueryDefinitions,
    PlanDefinitions,
    ResourceSamples,
    ActivityObservations,
    DiagnosticCheckpoints,
    DeadlockEvents,
    OperationalSamples,
    WaitCounterSamples,
    ReplicationSamples,
    SettingObservations,
    TuningObservations,
    ObjectDefinitions,
}

impl TelemetryStream {
    /// Checkpoints travel through exact private recovery; warehouse publication contains analytical records.
    pub fn delivery_required(self) -> bool {
        self != Self::DiagnosticCheckpoints
    }

    pub const ALL: [Self; 14] = [
        Self::QueryRuntime,
        Self::QueryWaits,
        Self::QueryDefinitions,
        Self::PlanDefinitions,
        Self::ResourceSamples,
        Self::ActivityObservations,
        Self::DiagnosticCheckpoints,
        Self::DeadlockEvents,
        Self::OperationalSamples,
        Self::WaitCounterSamples,
        Self::ReplicationSamples,
        Self::SettingObservations,
        Self::TuningObservations,
        Self::ObjectDefinitions,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            Self::QueryRuntime => "runtime.ndjson",
            Self::QueryWaits => "waits.ndjson",
            Self::QueryDefinitions => "queries.ndjson",
            Self::PlanDefinitions => "plans.ndjson",
            Self::ResourceSamples => "resources.ndjson",
            Self::ActivityObservations => "activity.ndjson",
            Self::DiagnosticCheckpoints => "diagnostic-state.ndjson",
            Self::DeadlockEvents => "deadlocks.ndjson",
            Self::OperationalSamples => "operations.ndjson",
            Self::WaitCounterSamples => "wait-counters.ndjson",
            Self::ReplicationSamples => "replication.ndjson",
            Self::SettingObservations => "settings.ndjson",
            Self::TuningObservations => "tuning.ndjson",
            Self::ObjectDefinitions => "objects.ndjson",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct TelemetryInterval {
    pub native_id: String,
    pub from: String,
    pub until: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceContinuity {
    Unverified,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "stream", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum TelemetryRecord {
    QueryRuntime(RuntimeMeasurement),
    QueryWaits(WaitMeasurement),
    #[serde(rename = "query-definitions")]
    QueryDefinition(QueryDefinition),
    #[serde(rename = "plan-definitions")]
    PlanDefinition(PlanDefinition),
    #[serde(rename = "resource-samples")]
    ResourceSample(ResourceMeasurement),
    #[serde(rename = "activity-observations")]
    Activity(Box<ActivityObservation>),
    #[serde(rename = "diagnostic-checkpoints")]
    DiagnosticCheckpoint(DiagnosticCheckpointPart),
    #[serde(rename = "deadlock-events")]
    Deadlock(DeadlockEvent),
    #[serde(rename = "operational-samples")]
    Operational(OperationalObservation),
    #[serde(rename = "wait-counter-samples")]
    WaitCounter(WaitCounterObservation),
    #[serde(rename = "replication-samples")]
    Replication(ReplicationObservation),
    #[serde(rename = "setting-observations")]
    Setting(SettingObservation),
    #[serde(rename = "tuning-observations")]
    Tuning(TuningObservation),
    #[serde(rename = "object-definitions")]
    ObjectDefinition(ObjectDefinitionRecord),
}

/// Identity within one source interval and observation; it does not prove continuity across source resets.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeRecordIdentity {
    Runtime(NativeIdentity),
    Waits { key: NativeIdentity, category: String },
    QueryDefinition(String),
    PlanDefinition(String),
    ResourceSample { at: String, replica_id: Option<String> },
    Activity(ActivityKey),
    DiagnosticCheckpoint(String),
    Deadlock { position: String, ordinal: u32 },
    Operational { source: String, key: String },
    WaitCounter { source: String, wait_type: String },
    Replication { source: String, key: String },
    Setting { source: String, namespace: String, name: String },
    Tuning { source: String, key: String },
    ObjectDefinition { source: String, key: String },
}

impl From<ActivityObservation> for TelemetryRecord {
    fn from(observation: ActivityObservation) -> Self {
        Self::Activity(Box::new(observation))
    }
}

impl TelemetryRecord {
    fn for_export(&self, source: &CollectionDescription) -> Result<Self> {
        if !source.observation.collection().streams().contains(&self.stream()) {
            return Err(CollectionStop::InvalidPage.error());
        }
        let mut record = self.clone();
        let valid_identity = match &mut record {
            Self::QueryDefinition(row) => {
                if let DefinitionPayload::Available { text, .. } = &row.sql {
                    row.sql = DefinitionPayload::sql(text, source.target.engine());
                }
                row.id == row.identity.definition_id(source)?
            }
            Self::PlanDefinition(row) => {
                if let DefinitionPayload::Available { text, .. } = &row.plan {
                    row.plan = DefinitionPayload::plan(text, source.target.engine());
                }
                row.id == row.identity.definition_id(source)?
                    && row
                        .query_definition_id
                        .as_ref()
                        .is_none_or(|id| row.identity.query.definition_id(source).is_ok_and(|expected| *id == expected))
            }
            Self::ResourceSample(row) => {
                row.at = canonical_time(&row.at)?;
                source.observation.accepts_sample(&row.at)
            }
            Self::Activity(row) => {
                row.redact(source.target.engine());
                source.observation.activity().is_some()
                    && source
                        .target
                        .database()
                        .is_none_or(|database| row.database_context_is_consistent(&database.database))
            }
            Self::DiagnosticCheckpoint(row) => match &source.observation {
                SourceObservation::Diagnostics(description) => {
                    row.is_valid()
                        && row.target == source.target
                        && row.input == description.parent
                        && row.controller_version == description.controller_version
                }
                _ => false,
            },
            Self::Deadlock(row) => {
                row.at = canonical_time(&row.at)?;
                row.redact(source.target.engine());
                source.observation.events().is_some_and(|history| history.accepts(&row.at))
            }
            Self::Operational(row) => {
                row.fact.redact(source.target.engine());
                let accepted = source.observation.accepts_snapshot_record(row.stream(), &row.source);
                match (&source.observation, &row.fact) {
                    (SourceObservation::Diagnostics(description), OperationalFact::DiagnosticRevision(revision)) => {
                        accepted
                            && revision.controller_version == description.controller_version
                            && revision.episode.scope.target.telemetry_id()? == source.target.telemetry_id()?
                    }
                    (SourceObservation::Diagnostics(_), _) | (_, OperationalFact::DiagnosticRevision(_)) => false,
                    _ => accepted,
                }
            }
            Self::ObjectDefinition(row) => {
                *row = row.for_export(source.target.engine())?;
                source.observation.accepts_snapshot_record(TelemetryStream::ObjectDefinitions, &row.source)
            }
            Self::WaitCounter(row) => {
                source.observation.accepts_snapshot_record(TelemetryStream::WaitCounterSamples, &row.source)
            }
            Self::Replication(row) => {
                source.observation.accepts_snapshot_record(TelemetryStream::ReplicationSamples, &row.source)
            }
            Self::Setting(row) => {
                source.observation.accepts_snapshot_record(TelemetryStream::SettingObservations, &row.source)
            }
            Self::Tuning(row) => {
                row.fact.redact(source.target.engine());
                source.observation.accepts_snapshot_record(TelemetryStream::TuningObservations, &row.source)
            }
            Self::QueryRuntime(row) => {
                row.canonicalize_instants()?;
                source.accepts_measurement(&row.key)
            }
            Self::QueryWaits(row) => source.accepts_measurement(&row.key),
        };
        if !valid_identity || !record.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(record)
    }
    pub fn native_identity(&self) -> NativeRecordIdentity {
        match self {
            Self::QueryRuntime(record) => NativeRecordIdentity::Runtime(record.key.native.clone()),
            Self::QueryWaits(record) => {
                NativeRecordIdentity::Waits { key: record.key.native.clone(), category: record.category.clone() }
            }
            Self::QueryDefinition(record) => NativeRecordIdentity::QueryDefinition(record.id.clone()),
            Self::PlanDefinition(record) => NativeRecordIdentity::PlanDefinition(record.id.clone()),
            Self::ResourceSample(row) => {
                NativeRecordIdentity::ResourceSample { at: row.at.clone(), replica_id: row.replica_id.clone() }
            }
            Self::Activity(row) => NativeRecordIdentity::Activity(row.key.clone()),
            Self::DiagnosticCheckpoint(row) => NativeRecordIdentity::DiagnosticCheckpoint(row.id.clone()),
            Self::Deadlock(row) => {
                NativeRecordIdentity::Deadlock { position: row.position.clone(), ordinal: row.graph_ordinal }
            }
            Self::Operational(row) => {
                NativeRecordIdentity::Operational { source: row.source.clone(), key: row.key.clone() }
            }
            Self::WaitCounter(row) => {
                NativeRecordIdentity::WaitCounter { source: row.source.clone(), wait_type: row.wait_type.clone() }
            }
            Self::Replication(row) => {
                NativeRecordIdentity::Replication { source: row.source.clone(), key: row.fact.key() }
            }
            Self::Tuning(row) => NativeRecordIdentity::Tuning { source: row.source.clone(), key: row.fact.key() },
            Self::ObjectDefinition(row) => {
                NativeRecordIdentity::ObjectDefinition { source: row.source.clone(), key: row.key.clone() }
            }
            Self::Setting(row) => NativeRecordIdentity::Setting {
                source: row.source.clone(),
                namespace: row.namespace.clone(),
                name: row.name.clone(),
            },
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::QueryRuntime(row) => row.is_valid(),
            Self::QueryWaits(row) => row.is_valid(),
            Self::QueryDefinition(row) => row.is_valid(),
            Self::PlanDefinition(row) => row.is_valid(),
            Self::ResourceSample(row) => row.is_valid(),
            Self::Activity(row) => row.is_valid(),
            Self::DiagnosticCheckpoint(row) => row.is_valid(),
            Self::Deadlock(row) => row.is_valid(),
            Self::Operational(row) => row.is_valid(),
            Self::WaitCounter(row) => row.is_valid(),
            Self::Replication(row) => row.is_valid(),
            Self::Setting(row) => row.is_valid(),
            Self::Tuning(row) => row.is_valid(),
            Self::ObjectDefinition(row) => row.is_valid(),
        }
    }

    pub fn stream(&self) -> TelemetryStream {
        match self {
            Self::QueryRuntime(_) => TelemetryStream::QueryRuntime,
            Self::QueryWaits(_) => TelemetryStream::QueryWaits,
            Self::QueryDefinition(_) => TelemetryStream::QueryDefinitions,
            Self::PlanDefinition(_) => TelemetryStream::PlanDefinitions,
            Self::ResourceSample(_) => TelemetryStream::ResourceSamples,
            Self::Activity(_) => TelemetryStream::ActivityObservations,
            Self::DiagnosticCheckpoint(_) => TelemetryStream::DiagnosticCheckpoints,
            Self::Deadlock(_) => TelemetryStream::DeadlockEvents,
            Self::Operational(row) => row.stream(),
            Self::WaitCounter(_) => TelemetryStream::WaitCounterSamples,
            Self::Replication(_) => TelemetryStream::ReplicationSamples,
            Self::Setting(_) => TelemetryStream::SettingObservations,
            Self::Tuning(_) => TelemetryStream::TuningObservations,
            Self::ObjectDefinition(_) => TelemetryStream::ObjectDefinitions,
        }
    }

    pub fn measurement_key(&self) -> Option<&NativeMeasurementKey> {
        match self {
            Self::QueryRuntime(record) => Some(&record.key),
            Self::QueryWaits(record) => Some(&record.key),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CollectionStop {
    Pending,
    Cancelled,
    CancellationUnconfirmed,
    TimedOut,
    PageLimit,
    QueryLimit,
    ByteLimit,
    RowLimit,
    Pressure,
    PressureUnavailable,
    SourceChanged,
    SourceUnavailable,
    /// The requested retained window has not closed at the native source yet; retry its exact bounds.
    SourceNotReady,
    StorageFailed,
    InvalidPage,
    MissingDefinitions,
    RetentionGap,
}

impl CollectionStop {
    pub fn error(self) -> Error {
        let error = match self {
            Self::TimedOut => crate::limits::timeout_error(),
            Self::CancellationUnconfirmed => {
                Error::new(ErrorCode::QueryFailed, t!("collection.cancellation_unconfirmed"))
            }
            Self::SourceNotReady => Error::new(ErrorCode::QueryFailed, t!("collection.source_not_ready")),
            _ => Error::new(ErrorCode::QueryFailed, t!("collection.source_stopped")),
        };
        error.with_detail(crate::ErrorDetail::CollectionStopped(self))
    }

    pub fn from_error(error: &Error) -> Self {
        if let Some(crate::ErrorDetail::CollectionStopped(reason)) = error.detail {
            return reason;
        }
        match error.code {
            ErrorCode::Timeout => Self::TimedOut,
            _ => Self::SourceUnavailable,
        }
    }
}

#[derive(Clone, Debug)]
pub enum CollectionRead {
    Page { records: Vec<TelemetryRecord>, next_cursor: Option<String> },
    Stopped(CollectionStop),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceShutdown {
    Quiescent,
    Unconfirmed,
}

/// A completed native read either verifies its advertised observation or closes a declared paged snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceCompletion {
    Verified,
    Snapshot(StateSnapshotDescription),
    Changed,
}
impl SourceCompletion {
    pub fn verified_if(unchanged: bool) -> Self {
        if unchanged { Self::Verified } else { Self::Changed }
    }
    pub fn is_verified(&self) -> bool {
        !matches!(self, Self::Changed)
    }
}

/// A collector owns one source session. A dropped caller must cancel/disconnect native work; never return an
/// uncertain session to a pool. Paged snapshots may deliver provisional batches from one owned statement; `finish`
/// must drain its completion and late errors before final coverage can authorize publication.
/// `describe` reports the source-native observation and retention evidence for the selected logical collection.
/// Sources do not schedule other databases, persist cursors or publish to destinations; the controller owns those steps.
#[async_trait]
pub trait TelemetrySource: Send {
    async fn describe(&mut self) -> Result<CollectionDescription>;
    fn query_count(&self) -> u32;
    /// None when the provider cannot measure its native read tasks.
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead>;
    /// Recheck native ownership/identity and finish coverage. Paged descriptors require a snapshot completion.
    async fn finish(&mut self) -> Result<SourceCompletion>;
    /// Finish cancellation/disconnect handling before a controller exits its runtime.
    async fn close(&mut self) -> SourceShutdown;
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", content = "reason", rename_all = "kebab-case")]
#[ts(export)]
pub enum CollectionStatus {
    Complete,
    Incomplete(CollectionStop),
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CollectedFile {
    pub batch_id: String,
    pub stream: TelemetryStream,
    pub semantics: TelemetrySemantics,
    pub file: String,
    pub status: CollectionStatus,
    pub error_code: Option<ErrorCode>,
    pub rows: u32,
    pub bytes: u32,
    pub pages: u32,
    pub sha256: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CollectionManifest {
    pub format_version: u32,
    pub capture: CaptureIdentity,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub request: CollectionRequest,
    pub description: Option<CollectionDescription>,
    /// Complete means the declared native streams finished. Reads are reconcilable observations, not an atomic
    /// snapshot; unavailable definition payloads and unverified continuity still limit analytical use.
    pub status: CollectionStatus,
    pub source_queries: u32,
    pub read_metrics: Option<ReadWorkMetrics>,
    pub directory_synced: bool,
    pub files: Vec<CollectedFile>,
    pub references: ReferenceCoverage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_and_time_bounds_fail_before_any_source_work() {
        let mut request = CollectionRequest {
            subject: crate::telemetry::CollectionSubject::Database,
            selection: crate::telemetry::CollectionSelection::QueryHistory {
                from: "2026-11-09T06:00:00Z".into(),
                until: "2026-11-09T07:00:00Z".into(),
            },
            budget: CollectionBudget::default(),
        };
        assert!(request.validate().is_ok());
        request.budget.page_rows = 0;
        assert!(request.validate().is_err());
        request.budget = CollectionBudget::default();
        request.selection = CollectionSelection::QueryHistory {
            from: "2026-11-09T06:00:00Z".into(),
            until: "2026-11-09T06:00:00Z".into(),
        };
        assert!(request.validate().is_err());
        request.selection = CollectionSelection::QueryHistory {
            from: "2026-11-09T06:00:00Z".into(),
            until: "2026-11-11T07:00:00Z".into(),
        };
        assert!(request.validate().is_err());
    }
    #[test]
    fn wait_categories_are_part_of_native_record_identity() {
        let key = NativeMeasurementKey {
            native: crate::telemetry::NativeIdentity::new("1")
                .qualified("plan_id", "2")
                .qualified("replica_group_id", "3")
                .qualified("execution_type", (0).to_string()),
            references: crate::telemetry::StatementReferences {
                query: Some(crate::telemetry::NativeDefinitionReference {
                    native: crate::telemetry::NativeIdentity::new("q"),
                    definition_id: Some("b".repeat(64)),
                }),
                plan: Some(crate::telemetry::NativeDefinitionReference {
                    native: crate::telemetry::NativeIdentity::new("2"),
                    definition_id: None,
                }),
            },
        };
        let mut wait = WaitMeasurement {
            key,
            category: "1".into(),
            unit: crate::telemetry::RuntimeMetricUnit::Milliseconds,
            total: crate::telemetry::MeasurementValue::Integer("9007199254740993".into()),
            average: Some(1.0),
            minimum: Some(crate::telemetry::MeasurementValue::Integer("1".into())),
            maximum: Some(crate::telemetry::MeasurementValue::Integer("2".into())),
        };
        let first = TelemetryRecord::QueryWaits(wait.clone());
        wait.category = "2".into();
        let second = TelemetryRecord::QueryWaits(wait);
        assert!(first.is_valid() && second.is_valid());
        assert_ne!(first.native_identity(), second.native_identity());
        assert_eq!(serde_json::to_value(first).unwrap()["data"]["total"]["value"], "9007199254740993");
    }
}

#[cfg(test)]
mod contracts_tests;
