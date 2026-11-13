//! Point observations retain native facts and per-source coverage, independently of displayed wording.
use super::*;
use crate::{FileKind, FileScope, SourceState, operations::SequenceCapacity};

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SnapshotDataset {
    StatementCounters,
    StatementDefinitions,
    Sequences,
    Cleanup,
    Operational,
    WaitCounters,
    Replication,
    Settings,
    PlanCacheSummary,
    Tuning,
    StatementGrants,
    CurrentRequestMemory,
    MemoryFailures,
    NativeFailures,
    IndexOperations,
    Tables,
    ForeignKeys,
    Columns,
    Statistics,
    Indexes,
    CheckConstraints,
    MissingIndexes,
    QueryCompilation,
    CachedRuntime,
    Objects,
    Triggers,
    Partitions,
    CachedDefinitions,
    RetainedPlans,
}
impl SnapshotDataset {
    pub fn stream(self) -> TelemetryStream {
        self.streams()[0]
    }
    pub fn streams(self) -> &'static [TelemetryStream] {
        match self {
            Self::StatementCounters
            | Self::Sequences
            | Self::Cleanup
            | Self::Operational
            | Self::PlanCacheSummary
            | Self::StatementGrants
            | Self::CurrentRequestMemory
            | Self::MemoryFailures
            | Self::NativeFailures
            | Self::IndexOperations
            | Self::Tables
            | Self::Statistics
            | Self::Indexes
            | Self::MissingIndexes
            | Self::QueryCompilation
            | Self::CachedRuntime => &[TelemetryStream::OperationalSamples],
            Self::WaitCounters => &[TelemetryStream::WaitCounterSamples],
            Self::Replication => &[TelemetryStream::ReplicationSamples],
            Self::Settings => &[TelemetryStream::SettingObservations],
            Self::Tuning => &[TelemetryStream::TuningObservations],
            Self::StatementDefinitions
            | Self::ForeignKeys
            | Self::Columns
            | Self::CheckConstraints
            | Self::Objects
            | Self::Triggers
            | Self::Partitions
            | Self::CachedDefinitions
            | Self::RetainedPlans => &[TelemetryStream::ObjectDefinitions],
        }
    }
}

/// Declares admissible native sources before a paged read knows its final counts or observation end.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PagedSnapshotDescription {
    pub dataset: SnapshotDataset,
    pub observed_from: String,
    pub sources: Vec<String>,
}
impl PagedSnapshotDescription {
    pub fn is_valid(&self) -> bool {
        let mut names = std::collections::BTreeSet::new();
        !self.sources.is_empty()
            && self.sources.len() <= 128
            && self.sources.iter().all(|name| valid_key(name) && names.insert(name))
            && self.observed_from.len() <= 64
            && DateTime::parse_from_rfc3339(&self.observed_from).is_ok()
    }
    pub fn accepts(&self, stream: TelemetryStream, source: &str) -> bool {
        self.dataset.stream() == stream && self.sources.iter().any(|name| name == source)
    }
    pub fn accepts_completion(&self, completion: &StateSnapshotDescription) -> bool {
        self.is_valid()
            && completion.is_valid()
            && self.dataset == completion.dataset
            && canonical_time(&self.observed_from).ok() == canonical_time(&completion.observed_from).ok()
            && self.sources.iter().collect::<std::collections::BTreeSet<_>>()
                == completion.sources.iter().map(|source| &source.source).collect::<std::collections::BTreeSet<_>>()
    }
}

#[cfg(test)]
mod paging_tests {
    use super::*;
    fn source() -> CollectionDescription {
        CollectionDescription {
            target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "sample", "fixture.invalid", 1433, None)
                .into(),
            source: "native-paged-state".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset: SnapshotDataset::Operational,
                observed_from: "2020-01-01T00:00:00Z".into(),
                sources: vec!["fixture".into()],
            }),
        }
    }
    fn completion() -> StateSnapshotDescription {
        StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T00:00:00Z".into(),
            observed_until: "2020-01-01T00:00:01Z".into(),
            records: 2,
            sources: vec![StateSourceCoverage::collected("fixture".into(), 2)],
        }
    }
    #[test]
    fn a_paged_descriptor_admits_only_declared_sources_and_cannot_claim_completed_coverage() {
        let before = source();
        let request = CollectionRequest {
            subject: crate::telemetry::CollectionSubject::Database,
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
            budget: CollectionBudget::default(),
        };
        assert!(before.matches_request(&request));
        assert!(before.observation.accepts_snapshot_record(TelemetryStream::OperationalSamples, "fixture"));
        assert!(!before.observation.accepts_snapshot_record(TelemetryStream::OperationalSamples, "unexpected"));
        assert!(!before.observation.accepts_snapshot_record(TelemetryStream::SettingObservations, "fixture"));
        assert!(!before.observation.covers_request());
        assert!(!before.observation.matches_file_counts(&[]));
        assert!(before.completed(SourceCompletion::Verified).is_err());
        assert!(before.completed(SourceCompletion::Changed).unwrap().is_none());
    }
    #[test]
    fn completion_adds_final_counts_and_times_without_changing_source_identity_or_scope() {
        let before = source();
        let completed = before.completed(SourceCompletion::Snapshot(completion())).unwrap().unwrap();
        assert_eq!(before.target, completed.target);
        assert_eq!(before.source, completed.source);
        assert_eq!(before.scoped_id("fixture", &"row").unwrap(), completed.scoped_id("fixture", &"row").unwrap());
        assert!(completed.observation.covers_request());
        for field in ["dataset", "sources", "start", "end", "total"] {
            let mut wrong = completion();
            match field {
                "dataset" => wrong.dataset = SnapshotDataset::Settings,
                "sources" => wrong.sources[0].source = "unexpected".into(),
                "start" => wrong.observed_from = "2019-12-31T23:59:59Z".into(),
                "end" => wrong.observed_until = "2019-12-31T23:59:59Z".into(),
                _ => wrong.records = 3,
            }
            assert!(before.completed(SourceCompletion::Snapshot(wrong)).is_err(), "{field}");
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct StateSourceCoverage {
    pub source: String,
    pub state: SourceState,
    pub records: u32,
    pub complete_scope: bool,
    pub error_code: Option<ErrorCode>,
    pub stopped: Option<CollectionStop>,
}
impl StateSourceCoverage {
    pub fn collected(source: String, records: u32) -> Self {
        Self::observed(source, records, true)
    }
    pub fn observed(source: String, records: u32, complete_scope: bool) -> Self {
        Self {
            source,
            state: if !complete_scope {
                SourceState::Partial
            } else if records == 0 {
                SourceState::Empty
            } else {
                SourceState::Collected
            },
            records,
            complete_scope,
            error_code: None,
            stopped: None,
        }
    }
    pub fn unavailable(source: String, error_code: ErrorCode, stopped: Option<CollectionStop>) -> Self {
        Self {
            source,
            state: SourceState::Unavailable,
            records: 0,
            complete_scope: false,
            error_code: Some(error_code),
            stopped,
        }
    }
    pub fn not_applicable(source: String) -> Self {
        Self {
            source,
            state: SourceState::NotApplicable,
            records: 0,
            complete_scope: false,
            error_code: None,
            stopped: None,
        }
    }
    fn is_valid(&self) -> bool {
        let observed = match self.state {
            SourceState::Collected => self.records > 0,
            SourceState::Empty => self.records == 0,
            SourceState::Partial => !self.complete_scope,
            SourceState::Unavailable | SourceState::NotApplicable => self.records == 0 && !self.complete_scope,
        };
        valid_key(&self.source)
            && observed
            && self.records <= 100_000
            && (!self.complete_scope || matches!(self.state, SourceState::Collected | SourceState::Empty))
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct StateSnapshotDescription {
    pub dataset: SnapshotDataset,
    pub observed_from: String,
    pub observed_until: String,
    pub records: u32,
    pub sources: Vec<StateSourceCoverage>,
}
impl StateSnapshotDescription {
    pub fn is_valid(&self) -> bool {
        let mut sources = std::collections::BTreeSet::new();
        !self.sources.is_empty()
            && self.sources.len() <= 128
            && self.observed_from.len() <= 64
            && self.observed_until.len() <= 64
            && self.sources.iter().all(|source| source.is_valid() && sources.insert(&source.source))
            && self.sources.iter().map(|source| u64::from(source.records)).sum::<u64>() == u64::from(self.records)
            && matches!((DateTime::parse_from_rfc3339(&self.observed_from),DateTime::parse_from_rfc3339(&self.observed_until)),(Ok(from),Ok(until)) if from<=until)
    }
    pub fn accepts(&self, stream: TelemetryStream, source_key: &str) -> bool {
        self.dataset.stream() == stream
            && self.sources.iter().any(|source| {
                source.source == source_key && matches!(source.state, SourceState::Collected | SourceState::Partial)
            })
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SpaceAllocationKind {
    UserObjects,
    InternalObjects,
    VersionStore,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct SpaceAllocation {
    pub kind: SpaceAllocationKind,
    pub bytes: Option<f64>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct OperationalSpace {
    pub kind: FileKind,
    pub scope: FileScope,
    pub native_name: Option<String>,
    pub total_bytes: Option<f64>,
    pub used_bytes: Option<f64>,
    pub limit_bytes: Option<f64>,
    pub growth_enabled: Option<bool>,
    pub reuse_wait: Option<String>,
    pub parts: Vec<SpaceAllocation>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum OperationalFact {
    Space(OperationalSpace),
    Sequence(SequenceCapacity),
    FileIo(FileIoCounters),
    Counter(NativeCounterSample),
    CounterSet(Box<NativeCounterSet>),
    CatalogCounters(Box<NativeCatalogCounters>),
    Retention(Box<RetentionObservation>),
    RetentionCoverage(RetentionCoverage),
    ExecutionGrant(Box<ExecutionGrantObservation>),
    MssqlOutOfMemory(Box<MssqlOutOfMemoryObservation>),
    MysqlErrorLog(Box<MysqlErrorLogObservation>),
    PostgresLog(Box<PostgresLogFailureObservation>),
    StatementCounters(Box<StatementCounterSample>),
    Queue(QueueSample),
    MssqlCleanupHorizon(MssqlCleanupHorizon),
    MssqlPersistentVersionStore(Box<MssqlPersistentVersionStore>),
    MssqlStatementGrants(Box<StatementGrantStatistics>),
    MssqlIndexOperations(Box<MssqlIndexOperations>),
    MssqlTable(Box<MssqlTableSnapshot>),
    MssqlStatistics(Box<MssqlStatisticsSnapshot>),
    MssqlIndex(Box<MssqlIndexSnapshot>),
    MssqlMissingIndex(Box<MssqlMissingIndexSnapshot>),
    MssqlQueryCompilation(Box<MssqlQueryCompilation>),
    MssqlCachedRuntime(Box<CachedStatementRuntime>),
    SuspectPages {
        count: String,
        /// The native datetime has no offset; it does not establish a UTC event timestamp.
        last_reported_time_without_offset: Option<String>,
        reset: crate::ResetMarker,
    },
}
impl OperationalFact {
    fn matches_key(&self, key: &str) -> bool {
        let expected = match self {
            Self::CounterSet(counters) => counters.key(),
            Self::CatalogCounters(counters) => counters.catalog.key(),
            Self::Retention(retainer) => retainer.key(),
            Self::RetentionCoverage(coverage) => coverage.key(),
            Self::ExecutionGrant(grant) => grant.key(),
            Self::MssqlOutOfMemory(observation) => observation.key(),
            Self::MysqlErrorLog(observation) => observation.key(),
            Self::PostgresLog(observation) => observation.key(),
            Self::StatementCounters(counters) => counters.key(),
            Self::MssqlStatementGrants(grants) => grants.context.identity.key(),
            Self::MssqlIndexOperations(index) => index.identity.key(),
            Self::MssqlTable(table) => table.identity.key(),
            Self::MssqlStatistics(statistics) => statistics.key(),
            Self::MssqlIndex(index) => index.key(),
            Self::MssqlMissingIndex(index) => index.key(),
            Self::MssqlQueryCompilation(compilation) => compilation.key(),
            Self::MssqlCachedRuntime(runtime) => runtime.context.identity.key(),
            Self::Space(_)
            | Self::Sequence(_)
            | Self::FileIo(_)
            | Self::Counter(_)
            | Self::Queue(_)
            | Self::MssqlCleanupHorizon(_)
            | Self::MssqlPersistentVersionStore(_)
            | Self::SuspectPages { .. } => return true,
        };
        expected.is_ok_and(|expected| expected == key)
    }
    pub(super) fn redact(&mut self, engine: crate::Engine) {
        match self {
            Self::MssqlIndex(index) => index.redact(engine),
            Self::CatalogCounters(counters) => counters.catalog.redact(engine),
            Self::ExecutionGrant(grant) => grant.redact(engine),
            // Only the explicitly named native memory-metadata columns are retained. They contain no SQL payload
            // or connection identity; malformed native JSON is qualified without inventing a SQL redaction pass.
            Self::MssqlOutOfMemory(_) => {}
            Self::MysqlErrorLog(observation) => observation.redact(),
            Self::PostgresLog(observation) => observation.redact(),
            Self::Space(_)
            | Self::Sequence(_)
            | Self::FileIo(_)
            | Self::Counter(_)
            | Self::StatementCounters(_)
            | Self::CounterSet(_)
            | Self::Retention(_)
            | Self::RetentionCoverage(_)
            | Self::Queue(_)
            | Self::MssqlCleanupHorizon(_)
            | Self::MssqlPersistentVersionStore(_)
            | Self::MssqlStatementGrants(_)
            | Self::MssqlIndexOperations(_)
            | Self::MssqlTable(_)
            | Self::MssqlStatistics(_)
            | Self::MssqlMissingIndex(_)
            | Self::MssqlQueryCompilation(_)
            | Self::MssqlCachedRuntime(_)
            | Self::SuspectPages { .. } => {}
        }
    }
    pub fn is_valid(&self) -> bool {
        let number = |value: f64| value.is_finite() && value >= 0.0;
        let name = |value: &str| value.len() <= 2048;
        match self {
            Self::Space(space) => {
                space.native_name.as_deref().is_none_or(name)
                    && space.reuse_wait.as_deref().is_none_or(name)
                    && [space.total_bytes, space.used_bytes, space.limit_bytes].into_iter().flatten().all(number)
                    && space.parts.len() <= 16
                    && space.parts.iter().all(|part| part.bytes.is_none_or(number))
            }
            Self::Sequence(sequence) => {
                name(&sequence.name)
                    && sequence.table.as_deref().is_none_or(name)
                    && [&sequence.current, &sequence.increment, &sequence.minimum, &sequence.maximum]
                        .into_iter()
                        .all(|value| value.len() <= 40 && value.parse::<i128>().is_ok())
            }
            Self::SuspectPages { count, last_reported_time_without_offset, reset } => {
                valid_counter(count)
                    && last_reported_time_without_offset.as_deref().is_none_or(|value| value.len() <= 64)
                    && valid_reset(reset)
            }
            Self::FileIo(counters) => counters.is_valid(),
            Self::Counter(counter) => counter.is_valid(),
            Self::StatementCounters(counters) => counters.is_valid(),
            Self::CounterSet(counters) => counters.is_valid(),
            Self::CatalogCounters(counters) => counters.is_valid(),
            Self::Retention(retainer) => retainer.is_valid(),
            Self::RetentionCoverage(coverage) => coverage.is_valid(),
            Self::ExecutionGrant(grant) => grant.is_valid(),
            Self::MssqlOutOfMemory(observation) => observation.is_valid(),
            Self::MysqlErrorLog(observation) => observation.is_valid(),
            Self::PostgresLog(observation) => observation.is_valid(),
            Self::Queue(queue) => queue.is_valid(),
            Self::MssqlCleanupHorizon(horizon) => horizon.is_valid(),
            Self::MssqlPersistentVersionStore(store) => store.is_valid(),
            Self::MssqlStatementGrants(grants) => grants.is_valid(),
            Self::MssqlIndexOperations(index) => index.is_valid(),
            Self::MssqlTable(table) => table.is_valid(),
            Self::MssqlStatistics(statistics) => statistics.is_valid(),
            Self::MssqlIndex(index) => index.is_valid(),
            Self::MssqlMissingIndex(index) => index.is_valid(),
            Self::MssqlQueryCompilation(compilation) => compilation.is_valid(),
            Self::MssqlCachedRuntime(runtime) => runtime.is_valid(),
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct OperationalObservation {
    pub source: String,
    pub key: String,
    pub fact: OperationalFact,
}
impl OperationalObservation {
    pub fn from_counters(source: impl Into<String>, counters: NativeCounterSet) -> Result<Self> {
        let observation =
            Self { source: source.into(), key: counters.key()?, fact: OperationalFact::CounterSet(Box::new(counters)) };
        if !observation.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(observation)
    }
    pub fn stream(&self) -> TelemetryStream {
        SnapshotDataset::Operational.stream()
    }
    pub fn is_valid(&self) -> bool {
        valid_key(&self.source)
            && valid_key(&self.key)
            && self.fact.is_valid()
            && self.fact.matches_key(&self.key)
            && match &self.fact {
                OperationalFact::Retention(retainer) => retainer.source == self.source,
                OperationalFact::RetentionCoverage(_) => self.source == RetentionCoverage::SOURCE,
                OperationalFact::ExecutionGrant(grant) => grant.source == self.source,
                _ => true,
            }
    }
}
/// A native nonzero counter row. Absence does not establish an observed zero or an unchanged reset epoch.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WaitCounterObservation {
    pub source: String,
    pub wait_type: String,
    pub scope: crate::MeasurementScope,
    pub reset: crate::ResetMarker,
    pub waiting_tasks_count: String,
    pub wait_time_ms: String,
    pub signal_wait_time_ms: String,
    pub max_wait_time_ms: String,
}
impl WaitCounterObservation {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.source)
            && valid_key(&self.wait_type)
            && [&self.waiting_tasks_count, &self.wait_time_ms, &self.signal_wait_time_ms, &self.max_wait_time_ms]
                .into_iter()
                .all(|value| valid_counter(value))
            && valid_reset(&self.reset)
    }
}
