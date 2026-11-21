//! A native interval, retained samples and event/counter semantics are different contracts.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TelemetryCollection {
    QueryHistory,
    ResourceHistory,
    DeadlockHistory,
    Activity,
    Diagnostics,
    Snapshot(SnapshotDataset),
}
impl TelemetryCollection {
    pub fn streams(self) -> &'static [TelemetryStream] {
        match self {
            Self::QueryHistory => &[
                TelemetryStream::QueryRuntime,
                TelemetryStream::QueryWaits,
                TelemetryStream::QueryDefinitions,
                TelemetryStream::PlanDefinitions,
            ],
            Self::ResourceHistory => &[TelemetryStream::ResourceSamples],
            Self::DeadlockHistory => &[TelemetryStream::DeadlockEvents],
            Self::Activity => &[TelemetryStream::ActivityObservations],
            Self::Diagnostics => &[TelemetryStream::OperationalSamples, TelemetryStream::DiagnosticCheckpoints],
            Self::Snapshot(dataset) => dataset.streams(),
        }
    }
    pub fn source_streams(self) -> &'static [TelemetryStream] {
        self.streams()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum CollectionSelection {
    SourceRead {
        focus: Box<SourceReadFocus>,
        execution: Box<NativeSourceReadScope>,
    },
    QueryHistory {
        from: String,
        until: String,
    },
    ResourceHistory {
        from: String,
        until: String,
    },
    DeadlockHistory {
        from: String,
        until: String,
    },
    Activity,
    FocusedBlocking {
        focus: Box<ActivityRequestFocus>,
    },
    CacheEntryMemory {
        focus: Box<CacheEntryMemoryFocus>,
    },
    TableMemory {
        target: Box<MssqlObjectIdentity>,
    },
    ColumnstoreRowgroups {
        target: Box<MssqlColumnstoreTarget>,
    },
    FullTextPopulations {
        target: Box<MssqlFullTextPopulationTarget>,
    },
    FocusedPageWait {
        focus: Box<ActivityWaitFocus>,
    },
    PageWaitTasks {
        activity: DiagnosticCaptureRef,
        original_context: Box<NativeReadContext>,
        original_requests: Vec<ActivityRequestSelector>,
    },
    PageWaitWriters {
        focus: Box<PageWaitWriterFocus>,
    },
    PageTaskHolders {
        focus: Box<PageTaskHolderFocus>,
    },
    FocusedRequestMemory {
        focus: Box<ActivityRequestFocus>,
    },
    MaintenanceProgress {
        focus: Box<ActivityRequestFocus>,
    },
    CurrentRequestWork {
        focus: Box<ActivityRequestFocus>,
    },
    FocusedCompilationAdmission {
        focus: Box<ActivityRequestFocus>,
    },
    SharedCompilationAdmissionFocus {
        focus: Box<ActivityRequestFocus>,
        partition: SourcePartition,
    },
    SharedPageWaitFocus {
        focus: Box<ActivityWaitFocus>,
        partition: SourcePartition,
    },
    Diagnostics {
        parent: DiagnosticCaptureRef,
        controller_version: u32,
        step_id: String,
    },
    Snapshot {
        dataset: SnapshotDataset,
    },
}
impl CollectionSelection {
    pub fn collection(&self) -> TelemetryCollection {
        match self {
            Self::SourceRead { focus, .. } => TelemetryCollection::Snapshot(focus.question.dataset()),
            Self::QueryHistory { .. } => TelemetryCollection::QueryHistory,
            Self::ResourceHistory { .. } => TelemetryCollection::ResourceHistory,
            Self::DeadlockHistory { .. } => TelemetryCollection::DeadlockHistory,
            Self::Activity | Self::FocusedBlocking { .. } => TelemetryCollection::Activity,
            Self::FocusedRequestMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestMemory),
            Self::MaintenanceProgress { .. } => TelemetryCollection::Snapshot(SnapshotDataset::MaintenanceProgress),
            Self::CurrentRequestWork { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestWork),
            Self::FocusedCompilationAdmission { .. } | Self::SharedCompilationAdmissionFocus { .. } => {
                TelemetryCollection::Snapshot(SnapshotDataset::CompilerAdmission)
            }
            Self::CacheEntryMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CacheEntryMemory),
            Self::TableMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::TableMemory),
            Self::ColumnstoreRowgroups { .. } => TelemetryCollection::Snapshot(SnapshotDataset::ColumnstoreRowgroups),
            Self::FullTextPopulations { .. } => TelemetryCollection::Snapshot(SnapshotDataset::FullTextPopulations),
            Self::FocusedPageWait { .. } | Self::SharedPageWaitFocus { .. } | Self::PageWaitTasks { .. } => {
                TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets)
            }
            Self::PageWaitWriters { .. } => TelemetryCollection::Snapshot(SnapshotDataset::PageWaitWriters),
            Self::PageTaskHolders { .. } => TelemetryCollection::Snapshot(SnapshotDataset::PageTaskHolders),
            Self::Diagnostics { .. } => TelemetryCollection::Diagnostics,
            Self::Snapshot { dataset } => TelemetryCollection::Snapshot(*dataset),
        }
    }

    pub fn focus(&self) -> Option<&ActivityRequestFocus> {
        match self {
            Self::FocusedPageWait { focus } | Self::SharedPageWaitFocus { focus, .. } => Some(&focus.participant),
            Self::PageWaitWriters { focus } => Some(&focus.waiter.participant),
            Self::PageTaskHolders { focus } => Some(&focus.participant),
            Self::FocusedRequestMemory { focus }
            | Self::FocusedBlocking { focus }
            | Self::MaintenanceProgress { focus }
            | Self::CurrentRequestWork { focus }
            | Self::FocusedCompilationAdmission { focus }
            | Self::SharedCompilationAdmissionFocus { focus, .. } => Some(focus),
            _ => None,
        }
    }

    /// The outbox/recovery owner must authenticate retained capture receipts before this work can begin.
    pub fn requires_authenticated_parent(&self) -> bool {
        self.focus().is_some()
            || matches!(self, Self::SourceRead { .. } | Self::PageWaitTasks { .. } | Self::Diagnostics { .. })
    }

    pub fn shared_partition(&self) -> Option<&SourcePartition> {
        match self {
            Self::SharedPageWaitFocus { partition, .. } | Self::SharedCompilationAdmissionFocus { partition, .. } => {
                Some(partition)
            }
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TelemetrySemantics {
    Interval,
    Sample,
    /// Native events need a source position; polling samples cannot establish event completeness.
    Event,
    /// Running totals require reset/epoch evidence before calculating a delta.
    CumulativeCounter,
    Definition,
    Checkpoint,
}
impl TelemetryStream {
    pub fn semantics(self) -> TelemetrySemantics {
        match self {
            Self::QueryRuntime | Self::QueryWaits => TelemetrySemantics::Interval,
            Self::ResourceSamples => TelemetrySemantics::Sample,
            Self::QueryDefinitions | Self::PlanDefinitions | Self::ObjectDefinitions => TelemetrySemantics::Definition,
            Self::ActivityObservations => TelemetrySemantics::Sample,
            Self::DeadlockEvents => TelemetrySemantics::Event,
            Self::DiagnosticCheckpoints => TelemetrySemantics::Checkpoint,
            Self::OperationalSamples => TelemetrySemantics::Sample,
            Self::WaitCounterSamples => TelemetrySemantics::CumulativeCounter,
            Self::ReplicationSamples => TelemetrySemantics::Sample,
            Self::SettingObservations => TelemetrySemantics::Sample,
            Self::TuningObservations => TelemetrySemantics::Sample,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum TelemetryRange {
    NativeInterval {
        interval: TelemetryInterval,
    },
    /// Half-open bounds on native sample/event timestamps, not an invented native interval ID.
    TimeWindow {
        from: String,
        until: String,
    },
    /// Latest confirmed current observation. Capture revisions, not clocks, choose its head.
    Current,
    /// Exact controller/focus work identity. It does not imply a native time interval or source continuity.
    Focused {
        key: String,
    },
}
impl TelemetryRange {
    pub fn canonical(&self) -> Result<Self> {
        Ok(match self {
            Self::NativeInterval { interval } => Self::NativeInterval {
                interval: TelemetryInterval {
                    native_id: interval.native_id.clone(),
                    from: canonical_time(&interval.from)?,
                    until: canonical_time(&interval.until)?,
                },
            },
            Self::TimeWindow { from, until } => {
                Self::TimeWindow { from: canonical_time(from)?, until: canonical_time(until)? }
            }
            Self::Current => Self::Current,
            Self::Focused { key } => {
                if !valid_digest(key) {
                    return Err(CollectionStop::InvalidPage.error());
                }
                Self::Focused { key: key.clone() }
            }
        })
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct QueryHistoryDescription {
    pub interval: TelemetryInterval,
    /// Actual native acquisition owner, independent of the data namespace and historical execution populations.
    /// None means the source did not acquire this capability; it never authorizes member-local work.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub acquisition_owner: Option<Box<NativeReadContext>>,
    /// Native execution groups available at capture time, independent of the connected physical compute member.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub population_catalog: Option<crate::NativeRuntimePopulationCatalog>,
    /// Explicit source-owned membership, independent of opaque native qualifier names and comparison cohorts.
    pub population_binding: QueryHistoryPopulationBinding,
    /// Source-owned native wait meaning frozen with this capture. None establishes neither a vocabulary nor zero waits.
    pub waits: Option<QueryWaitSemantics>,
    /// Dimensions required on every native measurement, with the admissible values observed by the source.
    pub dimensions: std::collections::BTreeMap<String, Vec<String>>,
    /// Audit facts interpreted by the native adapter, never by common collection or delivery policy.
    pub native_properties: std::collections::BTreeMap<String, String>,
}

pub const MAX_QUERY_HISTORY_DIMENSION_VALUES: usize = 100;

impl QueryHistoryDescription {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.interval.native_id)
            && self.acquisition_owner.as_ref().is_none_or(|owner| {
                owner.has_qualified_local_owner()
                    && owner.target.database().is_some()
                    && owner.database_writable.is_some()
            })
            && self.population_binding.is_valid(self.population_catalog.as_ref(), &self.dimensions)
            && self.population_catalog.as_ref().is_none_or(|catalog| {
                catalog.membership.matches_window(Some(&self.interval.from), Some(&self.interval.until))
            })
            && self.waits.as_ref().is_none_or(QueryWaitSemantics::is_valid)
            && self.dimensions.len() <= MAX_NATIVE_QUALIFIERS
            && super::native_identity::native_properties_valid(&self.native_properties)
            && self.dimensions.iter().all(|(name, values)| {
                let unique = values.iter().collect::<std::collections::BTreeSet<_>>();
                valid_key(name)
                    && !values.is_empty()
                    && values.len() <= MAX_QUERY_HISTORY_DIMENSION_VALUES
                    && unique.len() == values.len()
                    && values.iter().all(|value| valid_key(value))
            })
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SampleHistoryDescription {
    pub from: String,
    pub until: String,
    pub observed_at: String,
    /// Retained native timestamp bounds observed before paging. They do not prove continuity between samples.
    pub available_from: Option<String>,
    pub available_until: Option<String>,
    pub expected_samples: u32,
    /// None when the native source does not promise a sampling period.
    pub nominal_period_ms: Option<u32>,
}
impl SampleHistoryDescription {
    /// A delayed native head is recoverable; only a proven missing lower bound establishes retention loss.
    pub fn coverage_stop(&self) -> Option<CollectionStop> {
        if !self.is_valid() {
            return Some(CollectionStop::InvalidPage);
        }
        if self.covers_request() {
            return None;
        }
        let Ok(from) = DateTime::parse_from_rfc3339(&self.from) else {
            return Some(CollectionStop::InvalidPage);
        };
        if self
            .available_from
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|first| first > from)
        {
            Some(CollectionStop::RetentionGap)
        } else {
            Some(CollectionStop::SourceNotReady)
        }
    }
    pub fn covers_request(&self) -> bool {
        let parse = |value: &str| DateTime::parse_from_rfc3339(value).ok();
        match (
            parse(&self.from),
            parse(&self.until),
            self.available_from.as_deref().and_then(parse),
            self.available_until.as_deref().and_then(parse),
        ) {
            (Some(from), Some(until), Some(first), Some(last)) => first <= from && from < until && until <= last,
            _ => false,
        }
    }
    fn is_valid(&self) -> bool {
        let time = |value: &str| value.len() <= 64 && DateTime::parse_from_rfc3339(value).is_ok();
        time(&self.from)
            && time(&self.until)
            && time(&self.observed_at)
            && self.available_from.as_deref().is_none_or(time)
            && self.available_until.as_deref().is_none_or(time)
            && self.available_from.is_some() == self.available_until.is_some()
            && self.nominal_period_ms != Some(0)
            && DateTime::parse_from_rfc3339(&self.from).ok() < DateTime::parse_from_rfc3339(&self.until).ok()
            && DateTime::parse_from_rfc3339(&self.until).ok() <= DateTime::parse_from_rfc3339(&self.observed_at).ok()
            && match (&self.available_from, &self.available_until) {
                (Some(from), Some(until)) => {
                    DateTime::parse_from_rfc3339(from).ok() <= DateTime::parse_from_rfc3339(until).ok()
                        && DateTime::parse_from_rfc3339(until).ok()
                            <= DateTime::parse_from_rfc3339(&self.observed_at).ok()
                }
                _ => self.expected_samples == 0,
            }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticCaptureDescription {
    pub controller_version: u32,
    pub parent: DiagnosticCaptureRef,
    pub step_id: String,
    /// Analysis time on the collector; native observation clocks remain in referenced evidence.
    pub analyzed_at: String,
    pub records: u32,
    pub checkpoint_records: u32,
    /// Private recovery bindings for every native scope referenced by this update, including older epochs.
    /// Warehouse projections expose only these keys; exact native identities remain authenticated here.
    pub scope_bindings: std::collections::BTreeMap<String, DiagnosticScope>,
}
impl DiagnosticCaptureDescription {
    pub fn accepts_scope(&self, scope: &DiagnosticScope) -> bool {
        scope.key().is_ok_and(|key| self.scope_bindings.get(&key) == Some(scope))
    }
    pub fn is_valid(&self) -> bool {
        self.controller_version > 0
            && self.parent.is_valid()
            && valid_digest(&self.step_id)
            && canonical_time(&self.analyzed_at).is_ok()
            && self.records <= MAX_DIAGNOSTIC_EPISODES as u32
            && self.checkpoint_records > 0
            && self.checkpoint_records <= MAX_DIAGNOSTIC_EPISODES as u32
            && self.scope_bindings.len() <= MAX_DIAGNOSTIC_EPISODES
            && self.scope_bindings.iter().all(|(key, scope)| scope.key().is_ok_and(|expected| &expected == key))
    }
    pub fn work_key(&self) -> Result<String> {
        serde_json::to_vec(&(&self.parent, self.controller_version, &self.step_id))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceObservation {
    /// The dataset keeps its logical namespace. Execution is the original source-owned native read owner,
    /// separate from query/compile population identities and from the private incident retained in lineage.
    SourceRead {
        snapshot: StateSnapshotDescription,
        focus_key: String,
        execution: Box<NativeReadContext>,
    },
    PagedSourceRead {
        snapshot: PagedSnapshotDescription,
        focus_key: String,
        execution: Box<NativeReadContext>,
    },
    QueryHistory(QueryHistoryDescription),
    RetainedSamples(SampleHistoryDescription),
    RetainedEvents(EventHistoryDescription),
    Activity(ActivitySnapshotDescription),
    FocusedBlocking {
        snapshot: ActivitySnapshotDescription,
        result: Box<ActivityBlockingFocusResult>,
    },
    Jobs {
        snapshot: StateSnapshotDescription,
        metadata: Box<TimescaleJobsMetadata>,
    },
    MysqlEvents {
        snapshot: StateSnapshotDescription,
        metadata: Box<MysqlEventsMetadata>,
    },
    SuspectPages {
        snapshot: StateSnapshotDescription,
        native_database: Box<NativeIdentity>,
        database_writable: bool,
    },
    DatabasePerformanceCounters {
        snapshot: StateSnapshotDescription,
        native_database: Box<NativeIdentity>,
        database_writable: bool,
    },
    BackupHistory {
        snapshot: StateSnapshotDescription,
        scope: Box<MssqlBackupHistoryScope>,
        database_writable: bool,
    },
    MssqlAgentJobs {
        snapshot: StateSnapshotDescription,
        metadata: Box<MssqlAgentJobsMetadata>,
    },
    CacheEntryMemory {
        snapshot: StateSnapshotDescription,
        focus: Box<CacheEntryMemoryFocus>,
    },
    MssqlReplication {
        snapshot: StateSnapshotDescription,
        scope: Box<MssqlReplicationScope>,
        database_writable: bool,
    },
    GroupReplication {
        snapshot: StateSnapshotDescription,
        metadata: Box<MysqlGroupReplicationMetadata>,
    },
    Galera {
        snapshot: StateSnapshotDescription,
        metadata: Box<MariadbGaleraMetadata>,
    },
    TableMemory {
        snapshot: StateSnapshotDescription,
        target: Box<MssqlObjectIdentity>,
        metadata: Box<MssqlTableMemoryMetadata>,
        database_writable: bool,
    },
    ColumnstoreRowgroups {
        snapshot: StateSnapshotDescription,
        target: Box<MssqlColumnstoreTarget>,
        index_type: u8,
        database_writable: bool,
    },
    FullTextPopulations {
        snapshot: StateSnapshotDescription,
        target: Box<MssqlFullTextPopulationTarget>,
        metadata: Box<MssqlFullTextPopulationMetadata>,
        database_writable: bool,
    },
    /// Native page/task sample from one held source. Updateability belongs only to a database target.
    PageWaitTargets {
        snapshot: StateSnapshotDescription,
        database_writable: Option<bool>,
        /// Captured in the authenticated request before native work; None is an unpaired raw population.
        original_activity: Option<DiagnosticCaptureRef>,
    },
    FocusedPageWait {
        snapshot: StateSnapshotDescription,
        result: Box<ActivityWaitFocusResult>,
    },
    PageTaskHolders {
        snapshot: StateSnapshotDescription,
        focus: Box<PageTaskHolderFocus>,
        database_writable: Option<bool>,
    },
    PageWaitWriters {
        snapshot: StateSnapshotDescription,
        focus: Box<PageWaitWriterFocus>,
        database_writable: Option<bool>,
    },
    FocusedRequestMemory {
        snapshot: StateSnapshotDescription,
        result: Box<ActivityRequestMemoryFocusResult>,
    },
    MaintenanceProgress {
        snapshot: StateSnapshotDescription,
        result: Box<FocusedRequestResult>,
    },
    CurrentRequestWork {
        profile: CurrentRequestWorkProfile,
        snapshot: StateSnapshotDescription,
        result: Box<FocusedRequestResult>,
    },
    FocusedCompilationAdmission {
        snapshot: StateSnapshotDescription,
        result: Box<ActivityCompilationAdmissionFocusResult>,
    },
    Diagnostics(DiagnosticCaptureDescription),
    Snapshot(StateSnapshotDescription),
    PagedSnapshot(PagedSnapshotDescription),
}
impl SourceObservation {
    pub fn focus(&self) -> Option<&ActivityRequestFocus> {
        match self {
            Self::FocusedBlocking { result, .. } => Some(&result.focus),
            Self::FocusedPageWait { result, .. } => Some(&result.focus.participant),
            Self::PageWaitWriters { focus, .. } => Some(&focus.waiter.participant),
            Self::PageTaskHolders { focus, .. } => Some(&focus.participant),
            Self::FocusedRequestMemory { result, .. } => Some(&result.focus),
            Self::MaintenanceProgress { result, .. } | Self::CurrentRequestWork { result, .. } => {
                result.focus.as_deref()
            }
            Self::FocusedCompilationAdmission { result, .. } => Some(&result.focus),
            _ => None,
        }
    }
    pub fn diagnostic_focus(&self) -> Option<DiagnosticFocus> {
        match self {
            Self::FocusedBlocking { result, .. } => {
                Some(DiagnosticFocus::Blocking { focus: Box::new(result.focus.clone()) })
            }
            Self::CurrentRequestWork { result, .. } => result
                .focus
                .as_deref()
                .map(|focus| DiagnosticFocus::CurrentRequestWork { focus: Box::new(focus.clone()) }),
            Self::FocusedCompilationAdmission { result, .. } => {
                Some(DiagnosticFocus::CompilationAdmission { focus: Box::new(result.focus.clone()) })
            }
            Self::FocusedPageWait { result, .. } => Some(result.focus.clone().into()),
            Self::PageWaitWriters { focus, .. } => Some(DiagnosticFocus::PageWaitWriters { focus: focus.clone() }),
            Self::PageTaskHolders { focus, .. } => Some(DiagnosticFocus::PageTaskHolders { focus: focus.clone() }),
            Self::FocusedRequestMemory { result, .. } => {
                Some(DiagnosticFocus::RequestMemory { focus: Box::new(result.focus.clone()) })
            }
            _ => None,
        }
    }
    pub fn collection(&self) -> TelemetryCollection {
        match self {
            Self::FocusedCompilationAdmission { .. } => {
                TelemetryCollection::Snapshot(SnapshotDataset::CompilerAdmission)
            }
            Self::QueryHistory(_) => TelemetryCollection::QueryHistory,
            Self::RetainedSamples(_) => TelemetryCollection::ResourceHistory,
            Self::RetainedEvents(_) => TelemetryCollection::DeadlockHistory,
            Self::Activity(_) | Self::FocusedBlocking { .. } => TelemetryCollection::Activity,
            Self::CacheEntryMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CacheEntryMemory),
            Self::BackupHistory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::BackupHistory),
            Self::SuspectPages { .. } => TelemetryCollection::Snapshot(SnapshotDataset::SuspectPages),
            Self::DatabasePerformanceCounters { .. } => {
                TelemetryCollection::Snapshot(SnapshotDataset::DatabasePerformanceCounters)
            }
            Self::TableMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::TableMemory),
            Self::MssqlReplication { .. } => TelemetryCollection::Snapshot(SnapshotDataset::Replication),
            Self::GroupReplication { .. } => TelemetryCollection::Snapshot(SnapshotDataset::GroupReplication),
            Self::Galera { .. } => TelemetryCollection::Snapshot(SnapshotDataset::Galera),
            Self::ColumnstoreRowgroups { .. } => TelemetryCollection::Snapshot(SnapshotDataset::ColumnstoreRowgroups),
            Self::FullTextPopulations { .. } => TelemetryCollection::Snapshot(SnapshotDataset::FullTextPopulations),
            Self::PageWaitTargets { .. } | Self::FocusedPageWait { .. } => {
                TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets)
            }
            Self::PageWaitWriters { .. } => TelemetryCollection::Snapshot(SnapshotDataset::PageWaitWriters),
            Self::PageTaskHolders { .. } => TelemetryCollection::Snapshot(SnapshotDataset::PageTaskHolders),
            Self::FocusedRequestMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestMemory),
            Self::MaintenanceProgress { .. } => TelemetryCollection::Snapshot(SnapshotDataset::MaintenanceProgress),
            Self::CurrentRequestWork { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestWork),
            Self::Diagnostics(_) => TelemetryCollection::Diagnostics,
            Self::MysqlEvents { .. } | Self::Jobs { .. } | Self::MssqlAgentJobs { .. } => {
                TelemetryCollection::Snapshot(SnapshotDataset::Jobs)
            }
            Self::Snapshot(snapshot) | Self::SourceRead { snapshot, .. } => {
                TelemetryCollection::Snapshot(snapshot.dataset)
            }
            Self::PagedSnapshot(snapshot) | Self::PagedSourceRead { snapshot, .. } => {
                TelemetryCollection::Snapshot(snapshot.dataset)
            }
        }
    }
    pub fn range(&self) -> Result<TelemetryRange> {
        Ok(match self {
            Self::SourceRead { focus_key, .. } | Self::PagedSourceRead { focus_key, .. } => {
                TelemetryRange::Focused { key: focus_key.clone() }
            }
            Self::QueryHistory(history) => TelemetryRange::NativeInterval { interval: history.interval.clone() },
            Self::RetainedSamples(history) => {
                TelemetryRange::TimeWindow { from: history.from.clone(), until: history.until.clone() }
            }
            Self::RetainedEvents(history) => {
                TelemetryRange::TimeWindow { from: history.from.clone(), until: history.until.clone() }
            }
            Self::Activity(_) => TelemetryRange::Current,
            Self::FocusedBlocking { result, .. } => TelemetryRange::Focused { key: result.focus.key()? },
            Self::CacheEntryMemory { focus, .. } => TelemetryRange::Focused { key: focus.key()? },
            Self::TableMemory { target, .. } => TelemetryRange::Focused { key: target.key()? },
            Self::ColumnstoreRowgroups { target, .. } => TelemetryRange::Focused { key: target.key()? },
            Self::FullTextPopulations { target, .. } => TelemetryRange::Focused { key: target.key()? },
            Self::FocusedPageWait { result, .. } => TelemetryRange::Focused { key: result.focus.key()? },
            Self::PageWaitWriters { focus, .. } => TelemetryRange::Focused { key: focus.key()? },
            Self::PageTaskHolders { focus, .. } => TelemetryRange::Focused { key: focus.key()? },
            Self::FocusedRequestMemory { result, .. } => TelemetryRange::Focused { key: result.focus.key()? },
            Self::MaintenanceProgress { snapshot, result } | Self::CurrentRequestWork { snapshot, result, .. } => {
                match &result.focus {
                    Some(focus) => TelemetryRange::Focused { key: focus.key()? },
                    None => TelemetryRange::TimeWindow {
                        from: snapshot.observed_from.clone(),
                        until: snapshot.observed_until.clone(),
                    },
                }
            }
            Self::FocusedCompilationAdmission { result, .. } => TelemetryRange::Focused { key: result.focus.key()? },
            Self::Diagnostics(description) => TelemetryRange::Focused { key: description.work_key()? },
            Self::PageWaitTargets { original_activity: Some(activity), .. } => TelemetryRange::Focused {
                key: digest(
                    &serde_json::to_vec(&("page-wait-task-population-v1", activity))
                        .map_err(|_| CollectionStop::InvalidPage.error())?,
                ),
            },
            Self::PageWaitTargets { .. }
            | Self::Jobs { .. }
            | Self::MysqlEvents { .. }
            | Self::MssqlAgentJobs { .. }
            | Self::BackupHistory { .. }
            | Self::SuspectPages { .. }
            | Self::DatabasePerformanceCounters { .. }
            | Self::MssqlReplication { .. }
            | Self::GroupReplication { .. }
            | Self::Galera { .. }
            | Self::Snapshot(_)
            | Self::PagedSnapshot(_) => TelemetryRange::Current,
        })
    }
    pub fn accepts_sample(&self, at: &str) -> bool {
        let Self::RetainedSamples(history) = self else {
            return false;
        };
        let parse = |value: &str| DateTime::parse_from_rfc3339(value).ok();
        matches!((parse(&history.from), parse(at), parse(&history.until)), (Some(from),Some(at),Some(until)) if from <= at && at < until)
    }
    pub fn covers_request(&self) -> bool {
        match self {
            Self::SourceRead { snapshot, focus_key, execution } => {
                snapshot.is_valid()
                    && SourceReadQuestion::for_dataset(snapshot.dataset).is_some()
                    && valid_digest(focus_key)
                    && NativeSourceReadScope::from_context(execution).is_ok()
                    && same_time(&snapshot.observed_from, &execution.observed_from)
                    && same_time(&snapshot.observed_until, &execution.observed_until)
            }
            Self::QueryHistory(history) => history.is_valid(),
            Self::RetainedSamples(history) => history.covers_request(),
            // This proves a complete retained-source read; historical completeness is a separate explicit fact.
            Self::RetainedEvents(_) => true,
            Self::BackupHistory { snapshot, .. } => backup_history_header_is_valid(snapshot),
            Self::SuspectPages { snapshot, native_database, .. } => {
                suspect_pages_header_is_valid(snapshot) && validate_database_performance_identity(native_database)
            }
            Self::DatabasePerformanceCounters { snapshot, native_database, .. } => {
                database_performance_header_is_valid(snapshot)
                    && validate_database_performance_identity(native_database)
            }
            Self::Jobs { snapshot, metadata } => metadata.matches_snapshot(snapshot),
            Self::MssqlAgentJobs { snapshot, metadata } => metadata.matches_snapshot(snapshot),
            Self::MysqlEvents { snapshot, metadata } => metadata.matches_snapshot(snapshot),
            Self::Activity(snapshot) => snapshot.is_valid(),
            Self::FocusedBlocking { snapshot, result } => result.is_valid_header(snapshot),
            Self::MssqlReplication { snapshot, scope, .. } => {
                snapshot.is_valid()
                    && snapshot.dataset == SnapshotDataset::Replication
                    && scope.is_valid()
                    && snapshot.sources.iter().all(|source| {
                        !matches!(
                            source.source.as_str(),
                            MSSQL_HYPERSCALE_REPLICAS_SOURCE | MSSQL_AZURE_REPLICA_STATE_SOURCE
                        ) || !source.complete_scope
                    })
            }
            Self::GroupReplication { snapshot, metadata } => {
                group_replication_header_is_valid(snapshot) && metadata.is_valid()
            }
            Self::Galera { snapshot, metadata } => galera_header_is_valid(snapshot) && metadata.is_valid(),
            Self::TableMemory { snapshot, target, metadata, .. } => {
                table_memory_header_is_valid(snapshot) && target.is_valid() && metadata.is_valid()
            }
            Self::ColumnstoreRowgroups { snapshot, target, index_type, .. } => {
                columnstore_rowgroup_header_is_valid(snapshot) && target.is_valid() && matches!(*index_type, 5 | 6)
            }
            Self::FullTextPopulations { snapshot, target, metadata, .. } => {
                full_text_population_header_is_valid(snapshot, metadata) && target.is_valid()
            }
            Self::CacheEntryMemory { snapshot, focus } => {
                snapshot.is_valid() && focus.is_valid() && snapshot.dataset == SnapshotDataset::CacheEntryMemory
            }
            Self::PageWaitTargets { snapshot, original_activity, .. } => {
                original_activity.as_ref().is_none_or(DiagnosticCaptureRef::is_valid)
                    && snapshot.is_valid()
                    && snapshot.dataset == SnapshotDataset::PageWaitTargets
                    && snapshot.sources.iter().all(|source| !source.complete_scope)
            }
            Self::FocusedPageWait { snapshot, result } => snapshot.is_valid() && result.is_valid(),
            Self::PageTaskHolders { snapshot, focus, database_writable } => {
                snapshot.is_valid()
                    && snapshot.dataset == SnapshotDataset::PageTaskHolders
                    && snapshot.records == 1
                    && snapshot.sources.len() == 1
                    && !snapshot.sources[0].complete_scope
                    && focus.is_valid()
                    && database_writable.is_some()
                    && *database_writable == focus.participant.database_writable
                    && ObservationBounds::new(&focus.original_observed_until, &snapshot.observed_from).is_some()
            }
            Self::PageWaitWriters { snapshot, focus, database_writable } => {
                snapshot.is_valid()
                    && snapshot.dataset == SnapshotDataset::PageWaitWriters
                    && snapshot.records == 1
                    && focus.is_valid()
                    && database_writable.is_some()
                    && *database_writable == focus.waiter.participant.database_writable
            }
            Self::MaintenanceProgress { snapshot, result } => {
                snapshot.dataset == SnapshotDataset::MaintenanceProgress && result.is_valid_header(snapshot)
            }
            Self::CurrentRequestWork { profile, snapshot, result } => {
                snapshot.dataset == SnapshotDataset::CurrentRequestWork
                    && result.is_valid_header(snapshot)
                    && result.focus.as_deref().is_none_or(|focus| profile.accepts_focus(focus))
            }
            Self::FocusedRequestMemory { snapshot, result } => {
                snapshot.is_valid()
                    && result.is_valid()
                    && snapshot.dataset == SnapshotDataset::CurrentRequestMemory
                    && snapshot.records as usize
                        == result.peers.len() + usize::from(result.membership == RequestMemoryMembership::Observed)
                    && snapshot.observed_from == result.observed_from
                    && snapshot.observed_until == result.observed_until
            }
            Self::FocusedCompilationAdmission { snapshot, result } => {
                snapshot.is_valid()
                    && result.is_valid()
                    && snapshot.dataset == SnapshotDataset::CompilerAdmission
                    && snapshot.records as usize
                        == result.gateways.len()
                            + usize::from(result.membership == CompilationAdmissionMembership::Observed)
                    && snapshot.observed_from == result.observed_from
                    && snapshot.observed_until == result.observed_until
            }
            Self::Diagnostics(description) => description.is_valid(),
            Self::Snapshot(snapshot) => {
                !matches!(
                    snapshot.dataset,
                    SnapshotDataset::Jobs
                        | SnapshotDataset::CacheEntryMemory
                        | SnapshotDataset::MaintenanceProgress
                        | SnapshotDataset::CurrentRequestWork
                        | SnapshotDataset::PageWaitTargets
                        | SnapshotDataset::PageWaitWriters
                        | SnapshotDataset::PageTaskHolders
                ) && snapshot.is_valid()
            }
            Self::PagedSnapshot(_) | Self::PagedSourceRead { .. } => false,
        }
    }
    pub fn events(&self) -> Option<&EventHistoryDescription> {
        match self {
            Self::RetainedEvents(history) => Some(history),
            _ => None,
        }
    }
    pub fn activity(&self) -> Option<&ActivitySnapshotDescription> {
        match self {
            Self::Activity(snapshot) | Self::FocusedBlocking { snapshot, .. } => Some(snapshot),
            _ => None,
        }
    }
    pub fn snapshot(&self) -> Option<&StateSnapshotDescription> {
        match self {
            Self::Snapshot(snapshot)
            | Self::SourceRead { snapshot, .. }
            | Self::Jobs { snapshot, .. }
            | Self::MysqlEvents { snapshot, .. }
            | Self::MssqlAgentJobs { snapshot, .. }
            | Self::BackupHistory { snapshot, .. }
            | Self::SuspectPages { snapshot, .. }
            | Self::DatabasePerformanceCounters { snapshot, .. }
            | Self::PageWaitTargets { snapshot, .. }
            | Self::FocusedPageWait { snapshot, .. }
            | Self::PageWaitWriters { snapshot, .. }
            | Self::PageTaskHolders { snapshot, .. }
            | Self::FocusedRequestMemory { snapshot, .. }
            | Self::FocusedCompilationAdmission { snapshot, .. }
            | Self::MaintenanceProgress { snapshot, .. }
            | Self::CurrentRequestWork { snapshot, .. }
            | Self::ColumnstoreRowgroups { snapshot, .. }
            | Self::TableMemory { snapshot, .. }
            | Self::MssqlReplication { snapshot, .. }
            | Self::GroupReplication { snapshot, .. }
            | Self::Galera { snapshot, .. }
            | Self::FullTextPopulations { snapshot, .. }
            | Self::CacheEntryMemory { snapshot, .. } => Some(snapshot),
            _ => None,
        }
    }
    pub fn accepts_snapshot_record(&self, stream: TelemetryStream, source: &str) -> bool {
        match self {
            Self::Snapshot(snapshot)
            | Self::SourceRead { snapshot, .. }
            | Self::Jobs { snapshot, .. }
            | Self::MysqlEvents { snapshot, .. }
            | Self::MssqlAgentJobs { snapshot, .. }
            | Self::BackupHistory { snapshot, .. }
            | Self::SuspectPages { snapshot, .. }
            | Self::DatabasePerformanceCounters { snapshot, .. }
            | Self::PageWaitTargets { snapshot, .. }
            | Self::FocusedPageWait { snapshot, .. }
            | Self::PageWaitWriters { snapshot, .. }
            | Self::PageTaskHolders { snapshot, .. }
            | Self::FocusedRequestMemory { snapshot, .. }
            | Self::FocusedCompilationAdmission { snapshot, .. }
            | Self::MaintenanceProgress { snapshot, .. }
            | Self::CurrentRequestWork { snapshot, .. }
            | Self::ColumnstoreRowgroups { snapshot, .. }
            | Self::TableMemory { snapshot, .. }
            | Self::MssqlReplication { snapshot, .. }
            | Self::GroupReplication { snapshot, .. }
            | Self::Galera { snapshot, .. }
            | Self::FullTextPopulations { snapshot, .. }
            | Self::CacheEntryMemory { snapshot, .. } => snapshot.accepts(stream, source),
            Self::Diagnostics(_) => stream == TelemetryStream::OperationalSamples && source == DIAGNOSTICS_SOURCE,
            Self::PagedSnapshot(snapshot) | Self::PagedSourceRead { snapshot, .. } => snapshot.accepts(stream, source),
            _ => false,
        }
    }
    pub fn matches_file_counts(&self, files: &[CollectedFile]) -> bool {
        let count =
            |stream| files.iter().filter(|file| file.stream == stream).map(|file| u64::from(file.rows)).sum::<u64>();
        match self {
            Self::QueryHistory(_) | Self::RetainedEvents(_) => true,
            Self::RetainedSamples(history) => {
                count(TelemetryStream::ResourceSamples) == u64::from(history.expected_samples)
            }
            Self::Activity(snapshot) | Self::FocusedBlocking { snapshot, .. } => {
                count(TelemetryStream::ActivityObservations) == u64::from(snapshot.observations)
            }
            Self::PageWaitTargets { snapshot, .. }
            | Self::FocusedPageWait { snapshot, .. }
            | Self::PageWaitWriters { snapshot, .. }
            | Self::PageTaskHolders { snapshot, .. }
            | Self::FocusedRequestMemory { snapshot, .. }
            | Self::FocusedCompilationAdmission { snapshot, .. } => {
                count(TelemetryStream::OperationalSamples) == u64::from(snapshot.records)
            }
            Self::MaintenanceProgress { snapshot, .. } | Self::CurrentRequestWork { snapshot, .. } => {
                count(TelemetryStream::OperationalSamples) == u64::from(snapshot.records)
            }
            Self::Jobs { snapshot, .. }
            | Self::MysqlEvents { snapshot, .. }
            | Self::MssqlAgentJobs { snapshot, .. }
            | Self::BackupHistory { snapshot, .. }
            | Self::SuspectPages { snapshot, .. }
            | Self::DatabasePerformanceCounters { snapshot, .. }
            | Self::ColumnstoreRowgroups { snapshot, .. }
            | Self::TableMemory { snapshot, .. }
            | Self::GroupReplication { snapshot, .. }
            | Self::Galera { snapshot, .. }
            | Self::FullTextPopulations { snapshot, .. }
            | Self::CacheEntryMemory { snapshot, .. } => {
                count(TelemetryStream::OperationalSamples) == u64::from(snapshot.records)
            }
            Self::Diagnostics(description) => {
                count(TelemetryStream::OperationalSamples) == u64::from(description.records)
                    && count(TelemetryStream::DiagnosticCheckpoints) == u64::from(description.checkpoint_records)
            }
            Self::Snapshot(snapshot) | Self::SourceRead { snapshot, .. } | Self::MssqlReplication { snapshot, .. } => {
                count(snapshot.dataset.stream()) == u64::from(snapshot.records)
            }
            Self::PagedSnapshot(_) | Self::PagedSourceRead { .. } => false,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CollectionDescription {
    pub target: CollectionTarget,
    pub partition: SourcePartition,
    pub source: String,
    pub observation: SourceObservation,
    pub continuity: SourceContinuity,
}
impl CollectionDescription {
    /// Validate one row against an already qualified source description. Collection admission, manifest reopening
    /// and archived analysis validate the complete immutable metadata before iterating rows.
    pub fn accepts_measurement(&self, key: &NativeMeasurementKey) -> bool {
        self.is_partition_qualified()
            && self.query_history().is_some_and(|history| {
                history.population_binding.resolve(&key.native.qualifiers).is_ok()
                    && history.dimensions.iter().all(|(dimension, allowed)| {
                        key.native.qualifiers.get(dimension).is_some_and(|value| allowed.contains(value))
                    })
            })
    }

    pub fn completed(&self, completion: SourceCompletion) -> Result<Option<Self>> {
        if !self.partition_is_consistent() {
            return Err(CollectionStop::InvalidPage.error());
        }
        match completion {
            SourceCompletion::Changed => Ok(None),
            SourceCompletion::Verified
                if !matches!(
                    self.observation,
                    SourceObservation::PagedSnapshot(_) | SourceObservation::PagedSourceRead { .. }
                ) && !matches!(&self.observation,SourceObservation::QueryHistory(history) if history.acquisition_owner.is_some()) =>
            {
                Ok(Some(self.clone()))
            }
            SourceCompletion::QueryHistory(history) => {
                let SourceObservation::QueryHistory(initial) = &self.observation else {
                    return Err(CollectionStop::InvalidPage.error());
                };
                if !initial.accepts_completion(&history) {
                    return Err(CollectionStop::InvalidPage.error());
                }
                let mut completed = self.clone();
                completed.observation = SourceObservation::QueryHistory(*history);
                if !completed.partition_is_consistent() || !completed.observation.covers_request() {
                    return Err(CollectionStop::InvalidPage.error());
                }
                Ok(Some(completed))
            }
            SourceCompletion::SourceRead { snapshot, execution } => {
                let SourceObservation::PagedSourceRead { snapshot: declared, focus_key, execution: initial } =
                    &self.observation
                else {
                    return Err(CollectionStop::InvalidPage.error());
                };
                if !declared.accepts_completion(&snapshot)
                    || !NativeSourceReadScope::from_context(initial)?.matches_context(&execution)
                    || !same_time(&initial.observed_from, &execution.observed_from)
                    || ObservationBounds::new(&initial.observed_until, &execution.observed_until).is_none()
                {
                    return Err(CollectionStop::InvalidPage.error());
                }
                let mut completed = self.clone();
                completed.observation =
                    SourceObservation::SourceRead { snapshot, focus_key: focus_key.clone(), execution };
                if !completed.partition_is_consistent() || !completed.observation.covers_request() {
                    return Err(CollectionStop::InvalidPage.error());
                }
                Ok(Some(completed))
            }
            SourceCompletion::Snapshot(snapshot) => {
                let SourceObservation::PagedSnapshot(declared) = &self.observation else {
                    return Err(CollectionStop::InvalidPage.error());
                };
                if !declared.accepts_completion(&snapshot) {
                    return Err(CollectionStop::InvalidPage.error());
                }
                let mut completed = self.clone();
                completed.observation = SourceObservation::Snapshot(snapshot);
                if !completed.partition_is_consistent() {
                    return Err(CollectionStop::InvalidPage.error());
                }
                Ok(Some(completed))
            }
            SourceCompletion::Verified => Err(CollectionStop::InvalidPage.error()),
        }
    }
    pub fn query_history(&self) -> Option<&QueryHistoryDescription> {
        match &self.observation {
            SourceObservation::QueryHistory(history) => Some(history),
            _ => None,
        }
    }
    pub fn matches_request(&self, request: &CollectionRequest) -> bool {
        if request.validate().is_err()
            || self.source.is_empty()
            || !self.partition_is_consistent()
            || !request.subject.accepts(&self.target)
        {
            return false;
        }
        if let CollectionSelection::SourceRead { focus, execution: expected } = &request.selection {
            let Some(actual) = self.observation.execution_context() else {
                return false;
            };
            let (dataset, observed_rows, focus_key, valid) = match &self.observation {
                SourceObservation::SourceRead { snapshot, focus_key, .. } => {
                    (snapshot.dataset, snapshot.records, focus_key, self.observation.covers_request())
                }
                SourceObservation::PagedSourceRead { snapshot, focus_key, .. } => {
                    (snapshot.dataset, 0, focus_key, snapshot.is_valid())
                }
                _ => return false,
            };
            return valid
                && focus.question.accepts_dataset(dataset)
                && focus.key().ok().as_ref() == Some(focus_key)
                && expected.matches_context(actual)
                && expected.matches_target(&self.target)
                && self.partition == SourcePartition::Target
                && observed_rows <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::FocusedBlocking { snapshot, result },
            CollectionSelection::FocusedBlocking { focus },
        ) = (&self.observation, &request.selection)
        {
            return result.is_valid_source(self, snapshot)
                && &result.focus == focus.as_ref()
                && request.subject == CollectionSubject::Database
                && snapshot.coverage.source == self.source
                && snapshot.observations <= request.budget.max_snapshot_rows;
        }
        if let (SourceObservation::Activity(snapshot), CollectionSelection::Activity) =
            (&self.observation, &request.selection)
        {
            return snapshot.is_valid()
                && snapshot.coverage.source == self.source
                && snapshot.observations <= request.budget.max_snapshot_rows
                && (snapshot.native_rows <= request.budget.max_snapshot_rows
                    || snapshot.native_rows == request.budget.max_snapshot_rows.saturating_add(1)
                        && snapshot.coverage.state == crate::SourceState::Partial
                        && snapshot.coverage.stopped == Some(CollectionStop::RowLimit));
        }
        if let (
            SourceObservation::FocusedPageWait { snapshot, result },
            CollectionSelection::FocusedPageWait { focus } | CollectionSelection::SharedPageWaitFocus { focus, .. },
        ) = (&self.observation, &request.selection)
        {
            return snapshot.is_valid()
                && result.is_valid()
                && &result.focus == focus.as_ref()
                && focus.is_valid_for(&request.subject)
                && focus.matches_capture_scope(self)
                && request.selection.shared_partition().is_none_or(|partition| self.partition == *partition)
                && snapshot.dataset == SnapshotDataset::PageWaitTargets
                && snapshot.records <= request.budget.max_snapshot_rows
                && snapshot.observed_from == result.observed_from
                && snapshot.observed_until == result.observed_until;
        }
        if let (
            SourceObservation::FocusedCompilationAdmission { snapshot, result },
            CollectionSelection::FocusedCompilationAdmission { focus }
            | CollectionSelection::SharedCompilationAdmissionFocus { focus, .. },
        ) = (&self.observation, &request.selection)
        {
            return self.observation.covers_request()
                && &result.focus == focus.as_ref()
                && focus.matches_capture_scope(self)
                && request.selection.shared_partition().is_none_or(|partition| self.partition == *partition)
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::FocusedRequestMemory { snapshot, result },
            CollectionSelection::FocusedRequestMemory { focus },
        ) = (&self.observation, &request.selection)
        {
            return self.observation.covers_request()
                && &result.focus == focus.as_ref()
                && focus.matches_capture_scope(self)
                && request.subject == CollectionSubject::Database
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::PageTaskHolders { snapshot, focus: original, .. },
            CollectionSelection::PageTaskHolders { focus },
        ) = (&self.observation, &request.selection)
        {
            return self.observation.covers_request()
                && original == focus
                && request.subject == CollectionSubject::Database
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::PageWaitWriters { snapshot, focus: original, .. },
            CollectionSelection::PageWaitWriters { focus },
        ) = (&self.observation, &request.selection)
        {
            return self.observation.covers_request()
                && original == focus
                && request.subject == CollectionSubject::Database
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::MaintenanceProgress { snapshot, result },
            CollectionSelection::MaintenanceProgress { focus },
        )
        | (
            SourceObservation::CurrentRequestWork { snapshot, result, .. },
            CollectionSelection::CurrentRequestWork { focus },
        ) = (&self.observation, &request.selection)
        {
            return result.is_valid_source(self, snapshot)
                && result.focus.as_deref() == Some(focus.as_ref())
                && request.subject == CollectionSubject::Database
                && focus.target.telemetry_id().ok() == self.target.telemetry_id().ok()
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::Diagnostics(description),
            CollectionSelection::Diagnostics { parent, controller_version, step_id },
        ) = (&self.observation, &request.selection)
        {
            return description.is_valid()
                && &description.parent == parent
                && description.controller_version == *controller_version
                && &description.step_id == step_id
                && description.records.saturating_add(description.checkpoint_records)
                    <= request.budget.max_snapshot_rows;
        }
        if matches!(&request.selection, CollectionSelection::TableMemory { .. })
            || matches!(&self.observation, SourceObservation::TableMemory { .. })
        {
            let (
                SourceObservation::TableMemory { snapshot, target: original, .. },
                CollectionSelection::TableMemory { target },
            ) = (&self.observation, &request.selection)
            else {
                return false;
            };
            return original == target
                && table_memory_context(self).is_ok()
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::ColumnstoreRowgroups { snapshot, target: original, .. },
            CollectionSelection::ColumnstoreRowgroups { target },
        ) = (&self.observation, &request.selection)
        {
            return original == target
                && columnstore_rowgroup_context(self).is_ok()
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::FullTextPopulations { snapshot, target: original, .. },
            CollectionSelection::FullTextPopulations { target },
        ) = (&self.observation, &request.selection)
        {
            return original == target
                && full_text_population_context(self).is_ok()
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::CacheEntryMemory { snapshot, focus: original },
            CollectionSelection::CacheEntryMemory { focus },
        ) = (&self.observation, &request.selection)
        {
            return focus.is_valid()
                && original == focus
                && snapshot.is_valid()
                && snapshot.dataset == SnapshotDataset::CacheEntryMemory
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::Jobs { snapshot, metadata },
            CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        ) = (&self.observation, &request.selection)
        {
            return metadata.matches_source(self, snapshot) && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::MssqlAgentJobs { snapshot, metadata },
            CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        ) = (&self.observation, &request.selection)
        {
            return metadata.matches_source(self, snapshot) && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::MysqlEvents { snapshot, metadata },
            CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        ) = (&self.observation, &request.selection)
        {
            return metadata.matches_source(self, snapshot) && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let SourceObservation::PageWaitTargets { snapshot, original_activity, .. } = &self.observation {
            let associated = match &request.selection {
                CollectionSelection::PageWaitTasks { activity, original_context, .. } => {
                    request.subject == CollectionSubject::Database
                        && original_activity.as_ref() == Some(activity)
                        && NativeReadContext::from_snapshot(self)
                            .is_ok_and(|current| current.matches_earlier_scope(original_context))
                }
                CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets } => {
                    original_activity.is_none()
                }
                _ => false,
            };
            return associated
                && self.observation.covers_request()
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (
            SourceObservation::Snapshot(snapshot)
            | SourceObservation::BackupHistory { snapshot, .. }
            | SourceObservation::MssqlReplication { snapshot, .. }
            | SourceObservation::GroupReplication { snapshot, .. }
            | SourceObservation::Galera { snapshot, .. }
            | SourceObservation::SuspectPages { snapshot, .. }
            | SourceObservation::DatabasePerformanceCounters { snapshot, .. },
            CollectionSelection::Snapshot { dataset },
        ) = (&self.observation, &request.selection)
        {
            return self.observation.covers_request()
                && snapshot.dataset == *dataset
                && snapshot.records <= request.budget.max_snapshot_rows;
        }
        if let (SourceObservation::PagedSnapshot(snapshot), CollectionSelection::Snapshot { dataset }) =
            (&self.observation, &request.selection)
        {
            return snapshot.is_valid() && snapshot.dataset == *dataset;
        }
        let Ok((from, until)) = request.bounds() else {
            return false;
        };
        let expected = TelemetryRange::TimeWindow { from: from.to_rfc3339(), until: until.to_rfc3339() }.canonical();
        let (start, end, valid) = match (&self.observation, request.collection()) {
            (SourceObservation::QueryHistory(history), TelemetryCollection::QueryHistory) => {
                (&history.interval.from, &history.interval.until, history.is_valid())
            }
            (SourceObservation::RetainedSamples(history), TelemetryCollection::ResourceHistory) => {
                (&history.from, &history.until, history.is_valid())
            }
            (SourceObservation::RetainedEvents(history), TelemetryCollection::DeadlockHistory) => {
                (&history.from, &history.until, history.is_valid())
            }
            _ => return false,
        };
        valid
            && !self.source.is_empty()
            && self.target.is_valid()
            && TelemetryRange::TimeWindow { from: start.clone(), until: end.clone() }.canonical().ok() == expected.ok()
    }
}
