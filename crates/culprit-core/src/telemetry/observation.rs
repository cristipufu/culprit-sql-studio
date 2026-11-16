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
    QueryHistory { from: String, until: String },
    ResourceHistory { from: String, until: String },
    DeadlockHistory { from: String, until: String },
    Activity,
    CacheEntryMemory { focus: Box<CacheEntryMemoryFocus> },
    FocusedPageWait { focus: Box<ActivityWaitFocus> },
    PageWaitWriters { focus: Box<PageWaitWriterFocus> },
    FocusedRequestMemory { focus: Box<ActivityRequestFocus> },
    MaintenanceProgress { focus: Box<ActivityRequestFocus> },
    FocusedCompilationAdmission { focus: Box<ActivityRequestFocus> },
    SharedCompilationAdmissionFocus { focus: Box<ActivityRequestFocus>, partition: SourcePartition },
    SharedPageWaitFocus { focus: Box<ActivityWaitFocus>, partition: SourcePartition },
    Diagnostics { parent: DiagnosticCaptureRef, controller_version: u32, step_id: String },
    Snapshot { dataset: SnapshotDataset },
}
impl CollectionSelection {
    pub fn collection(&self) -> TelemetryCollection {
        match self {
            Self::QueryHistory { .. } => TelemetryCollection::QueryHistory,
            Self::ResourceHistory { .. } => TelemetryCollection::ResourceHistory,
            Self::DeadlockHistory { .. } => TelemetryCollection::DeadlockHistory,
            Self::Activity => TelemetryCollection::Activity,
            Self::FocusedRequestMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestMemory),
            Self::MaintenanceProgress { .. } => TelemetryCollection::Snapshot(SnapshotDataset::MaintenanceProgress),
            Self::FocusedCompilationAdmission { .. } | Self::SharedCompilationAdmissionFocus { .. } => {
                TelemetryCollection::Snapshot(SnapshotDataset::CompilerAdmission)
            }
            Self::CacheEntryMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CacheEntryMemory),
            Self::FocusedPageWait { .. } | Self::SharedPageWaitFocus { .. } => {
                TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets)
            }
            Self::PageWaitWriters { .. } => TelemetryCollection::Snapshot(SnapshotDataset::PageWaitWriters),
            Self::Diagnostics { .. } => TelemetryCollection::Diagnostics,
            Self::Snapshot { dataset } => TelemetryCollection::Snapshot(*dataset),
        }
    }

    pub fn focus(&self) -> Option<&ActivityRequestFocus> {
        match self {
            Self::FocusedPageWait { focus } | Self::SharedPageWaitFocus { focus, .. } => Some(&focus.participant),
            Self::PageWaitWriters { focus } => Some(&focus.waiter.participant),
            Self::FocusedRequestMemory { focus }
            | Self::MaintenanceProgress { focus }
            | Self::FocusedCompilationAdmission { focus }
            | Self::SharedCompilationAdmissionFocus { focus, .. } => Some(focus),
            _ => None,
        }
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

const MAX_DIMENSION_VALUES: usize = 100;

impl QueryHistoryDescription {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.interval.native_id)
            && self.population_binding.is_valid(self.population_catalog.as_ref(), &self.dimensions)
            && self.waits.as_ref().is_none_or(QueryWaitSemantics::is_valid)
            && self.dimensions.len() <= MAX_NATIVE_QUALIFIERS
            && super::native_identity::native_properties_valid(&self.native_properties)
            && self.dimensions.iter().all(|(name, values)| {
                let unique = values.iter().collect::<std::collections::BTreeSet<_>>();
                valid_key(name)
                    && !values.is_empty()
                    && values.len() <= MAX_DIMENSION_VALUES
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
    QueryHistory(QueryHistoryDescription),
    RetainedSamples(SampleHistoryDescription),
    RetainedEvents(EventHistoryDescription),
    Activity(ActivitySnapshotDescription),
    CacheEntryMemory {
        snapshot: StateSnapshotDescription,
        focus: Box<CacheEntryMemoryFocus>,
    },
    FocusedPageWait {
        snapshot: StateSnapshotDescription,
        result: Box<ActivityWaitFocusResult>,
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
        result: Box<MaintenanceProgressResult>,
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
            Self::FocusedPageWait { result, .. } => Some(&result.focus.participant),
            Self::PageWaitWriters { focus, .. } => Some(&focus.waiter.participant),
            Self::FocusedRequestMemory { result, .. } => Some(&result.focus),
            Self::MaintenanceProgress { result, .. } => result.focus.as_deref(),
            Self::FocusedCompilationAdmission { result, .. } => Some(&result.focus),
            _ => None,
        }
    }
    pub fn diagnostic_focus(&self) -> Option<DiagnosticFocus> {
        match self {
            Self::FocusedCompilationAdmission { result, .. } => {
                Some(DiagnosticFocus::CompilationAdmission { focus: Box::new(result.focus.clone()) })
            }
            Self::FocusedPageWait { result, .. } => Some(result.focus.clone().into()),
            Self::PageWaitWriters { focus, .. } => Some(DiagnosticFocus::PageWaitWriters { focus: focus.clone() }),
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
            Self::Activity(_) => TelemetryCollection::Activity,
            Self::CacheEntryMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CacheEntryMemory),
            Self::FocusedPageWait { .. } => TelemetryCollection::Snapshot(SnapshotDataset::PageWaitTargets),
            Self::PageWaitWriters { .. } => TelemetryCollection::Snapshot(SnapshotDataset::PageWaitWriters),
            Self::FocusedRequestMemory { .. } => TelemetryCollection::Snapshot(SnapshotDataset::CurrentRequestMemory),
            Self::MaintenanceProgress { .. } => TelemetryCollection::Snapshot(SnapshotDataset::MaintenanceProgress),
            Self::Diagnostics(_) => TelemetryCollection::Diagnostics,
            Self::Snapshot(snapshot) => TelemetryCollection::Snapshot(snapshot.dataset),
            Self::PagedSnapshot(snapshot) => TelemetryCollection::Snapshot(snapshot.dataset),
        }
    }
    pub fn range(&self) -> Result<TelemetryRange> {
        Ok(match self {
            Self::QueryHistory(history) => TelemetryRange::NativeInterval { interval: history.interval.clone() },
            Self::RetainedSamples(history) => {
                TelemetryRange::TimeWindow { from: history.from.clone(), until: history.until.clone() }
            }
            Self::RetainedEvents(history) => {
                TelemetryRange::TimeWindow { from: history.from.clone(), until: history.until.clone() }
            }
            Self::Activity(_) => TelemetryRange::Current,
            Self::CacheEntryMemory { focus, .. } => TelemetryRange::Focused { key: focus.key()? },
            Self::FocusedPageWait { result, .. } => TelemetryRange::Focused { key: result.focus.key()? },
            Self::PageWaitWriters { focus, .. } => TelemetryRange::Focused { key: focus.key()? },
            Self::FocusedRequestMemory { result, .. } => TelemetryRange::Focused { key: result.focus.key()? },
            Self::MaintenanceProgress { snapshot, result } => match &result.focus {
                Some(focus) => TelemetryRange::Focused { key: focus.key()? },
                None => TelemetryRange::TimeWindow {
                    from: snapshot.observed_from.clone(),
                    until: snapshot.observed_until.clone(),
                },
            },
            Self::FocusedCompilationAdmission { result, .. } => TelemetryRange::Focused { key: result.focus.key()? },
            Self::Diagnostics(description) => TelemetryRange::Focused { key: description.work_key()? },
            Self::Snapshot(_) | Self::PagedSnapshot(_) => TelemetryRange::Current,
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
            Self::QueryHistory(history) => history.is_valid(),
            Self::RetainedSamples(history) => history.covers_request(),
            // This proves a complete retained-source read; historical completeness is a separate explicit fact.
            Self::RetainedEvents(_) => true,
            Self::Activity(snapshot) => snapshot.is_valid(),
            Self::CacheEntryMemory { snapshot, focus } => {
                snapshot.is_valid() && focus.is_valid() && snapshot.dataset == SnapshotDataset::CacheEntryMemory
            }
            Self::FocusedPageWait { snapshot, result } => snapshot.is_valid() && result.is_valid(),
            Self::PageWaitWriters { snapshot, focus, database_writable } => {
                snapshot.is_valid()
                    && snapshot.dataset == SnapshotDataset::PageWaitWriters
                    && snapshot.records == 1
                    && focus.is_valid()
                    && database_writable.is_some()
                    && *database_writable == focus.waiter.participant.database_writable
            }
            Self::MaintenanceProgress { snapshot, result } => result.is_valid_header(snapshot),
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
                    SnapshotDataset::CacheEntryMemory
                        | SnapshotDataset::MaintenanceProgress
                        | SnapshotDataset::PageWaitWriters
                ) && snapshot.is_valid()
            }
            Self::PagedSnapshot(_) => false,
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
            Self::Activity(snapshot) => Some(snapshot),
            _ => None,
        }
    }
    pub fn snapshot(&self) -> Option<&StateSnapshotDescription> {
        match self {
            Self::Snapshot(snapshot)
            | Self::FocusedPageWait { snapshot, .. }
            | Self::PageWaitWriters { snapshot, .. }
            | Self::FocusedRequestMemory { snapshot, .. }
            | Self::FocusedCompilationAdmission { snapshot, .. }
            | Self::MaintenanceProgress { snapshot, .. }
            | Self::CacheEntryMemory { snapshot, .. } => Some(snapshot),
            _ => None,
        }
    }
    pub fn accepts_snapshot_record(&self, stream: TelemetryStream, source: &str) -> bool {
        match self {
            Self::Snapshot(snapshot)
            | Self::FocusedPageWait { snapshot, .. }
            | Self::PageWaitWriters { snapshot, .. }
            | Self::FocusedRequestMemory { snapshot, .. }
            | Self::FocusedCompilationAdmission { snapshot, .. }
            | Self::MaintenanceProgress { snapshot, .. }
            | Self::CacheEntryMemory { snapshot, .. } => snapshot.accepts(stream, source),
            Self::Diagnostics(_) => stream == TelemetryStream::OperationalSamples && source == DIAGNOSTICS_SOURCE,
            Self::PagedSnapshot(snapshot) => snapshot.accepts(stream, source),
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
            Self::Activity(snapshot) => {
                count(TelemetryStream::ActivityObservations) == u64::from(snapshot.observations)
            }
            Self::FocusedPageWait { snapshot, .. }
            | Self::PageWaitWriters { snapshot, .. }
            | Self::FocusedRequestMemory { snapshot, .. }
            | Self::FocusedCompilationAdmission { snapshot, .. } => {
                count(TelemetryStream::OperationalSamples) == u64::from(snapshot.records)
            }
            Self::MaintenanceProgress { snapshot, .. } => {
                count(TelemetryStream::OperationalSamples) == u64::from(snapshot.records)
            }
            Self::CacheEntryMemory { snapshot, .. } => {
                count(TelemetryStream::OperationalSamples) == u64::from(snapshot.records)
            }
            Self::Diagnostics(description) => {
                count(TelemetryStream::OperationalSamples) == u64::from(description.records)
                    && count(TelemetryStream::DiagnosticCheckpoints) == u64::from(description.checkpoint_records)
            }
            Self::Snapshot(snapshot) => count(snapshot.dataset.stream()) == u64::from(snapshot.records),
            Self::PagedSnapshot(_) => false,
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
            SourceCompletion::Verified if !matches!(self.observation, SourceObservation::PagedSnapshot(_)) => {
                Ok(Some(self.clone()))
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
        if let (SourceObservation::Snapshot(snapshot), CollectionSelection::Snapshot { dataset }) =
            (&self.observation, &request.selection)
        {
            return snapshot.is_valid()
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
