//! Durable diagnostic state references immutable native evidence. Lifecycle and analysis are independent facts.
use super::*;
use crate::SourceState;
use std::collections::BTreeSet;
mod transition;

pub const MAX_DIAGNOSTIC_EPISODES: usize = 4_096;
pub const MAX_DIAGNOSTIC_INPUTS: usize = 256;
pub const DIAGNOSTIC_CONTROLLER_VERSION: u32 = 3;
pub const DIAGNOSTICS_SOURCE: &str = "culprit-diagnostics";

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticCaptureRef {
    pub capture: CaptureIdentity,
    /// Original local CollectionManifest digest, attested by the recovery manifest when archived.
    pub manifest_sha256: String,
    pub source_scope_id: String,
}
impl DiagnosticCaptureRef {
    pub fn is_valid(&self) -> bool {
        valid_capture(&self.capture) && valid_digest(&self.manifest_sha256) && valid_digest(&self.source_scope_id)
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        digest_value(&(&self.capture, &self.manifest_sha256, &self.source_scope_id))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticEvidenceRef {
    pub capture: CaptureIdentity,
    pub manifest_sha256: String,
    pub source_scope_id: String,
    pub stream: TelemetryStream,
    pub native_key: NativeRecordIdentity,
    pub batch_id: String,
    pub record_id: String,
    pub payload_sha256: String,
    pub representation_version: u32,
    pub row_ordinal: u32,
}
impl DiagnosticEvidenceRef {
    pub fn capture_ref(&self) -> DiagnosticCaptureRef {
        DiagnosticCaptureRef {
            capture: self.capture.clone(),
            manifest_sha256: self.manifest_sha256.clone(),
            source_scope_id: self.source_scope_id.clone(),
        }
    }
    pub fn is_valid(&self) -> bool {
        self.capture_ref().is_valid()
            && valid_digest(&self.batch_id)
            && batch_id(&self.capture.observation_id, self.stream).is_ok_and(|expected| expected == self.batch_id)
            && valid_digest(&self.record_id)
            && valid_digest(&self.payload_sha256)
            && self.representation_version == REPRESENTATION_VERSION
            && self.native_key.stream() == self.stream
            && self.native_key.is_valid()
    }
    pub fn from_export(
        source: &CollectionDescription,
        manifest_sha256: &str,
        row: &ExportedRecord,
        row_ordinal: u32,
    ) -> Result<Self> {
        let expected = ExportedRecord::new(source, &row.capture, row.record.clone())?;
        if &expected != row || !row.record.is_valid() {
            return Err(invalid());
        }
        let value = Self {
            capture: row.capture.clone(),
            manifest_sha256: manifest_sha256.into(),
            source_scope_id: source.captured_range()?.scope_id,
            stream: row.record.stream(),
            native_key: row.record.native_identity(),
            batch_id: row.batch_id.clone(),
            record_id: row.record_id.clone().ok_or_else(invalid)?,
            payload_sha256: row.payload_sha256()?,
            representation_version: row.representation_version,
            row_ordinal,
        };
        if !value.is_valid() {
            return Err(invalid());
        }
        Ok(value)
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        digest_value(self)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticScope {
    pub target: CollectionTarget,
    pub partition: SourcePartition,
    pub source: String,
    pub population: ActivityPopulation,
    pub measurement_scope: crate::MeasurementScope,
    /// A verified native generation, never a collector epoch or a fabricated uptime identity.
    pub native_epoch: Option<String>,
}
impl DiagnosticScope {
    pub fn is_valid(&self) -> bool {
        self.partition.is_valid_for(&self.target)
            && valid_key(&self.source)
            && self.native_epoch.as_deref().is_none_or(valid_key)
            && match &self.partition {
                SourcePartition::Target => true,
                SourcePartition::InstanceLocal { epoch, .. } => epoch == &self.native_epoch,
            }
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        let native =
            (self.target.telemetry_id()?, &self.source, &self.native_epoch, self.population, self.measurement_scope);
        match &self.partition {
            SourcePartition::Target => digest_value(&native),
            partition => digest_value(&(partition, native)),
        }
    }
    pub fn same_member(&self, other: &Self) -> bool {
        self.is_valid()
            && other.is_valid()
            && self.target.telemetry_id().ok() == other.target.telemetry_id().ok()
            && self.source == other.source
            && self.partition.same_member(&other.partition)
    }
    pub fn from_source(source: &CollectionDescription) -> Result<Self> {
        if !source.partition_is_consistent() {
            return Err(invalid());
        }
        let frame = source.observation.activity().filter(|frame| frame.is_valid()).ok_or_else(invalid)?;
        let value = Self {
            target: source.target.clone(),
            partition: source.partition.clone(),
            source: source.source.clone(),
            native_epoch: frame.epoch.clone(),
            population: frame.population,
            measurement_scope: frame.scope,
        };
        if !value.is_valid() {
            return Err(invalid());
        }
        Ok(value)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DiagnosticLifecycle {
    Active,
    Uncertain,
    Resolved,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DiagnosticAnalysisStatus {
    Pending,
    Deferred,
    Collecting,
    Partial,
    Analyzed,
    Unresolved,
}
impl DiagnosticAnalysisStatus {
    pub fn needs_work(self) -> bool {
        matches!(self, Self::Pending | Self::Deferred | Self::Collecting | Self::Partial)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticAnalysisIssue {
    CostUnqualified,
    PolicyChanged,
    MissingNativeIdentity,
    NativeContextGone,
    SourceChanged,
    SourceUnavailable { code: Option<ErrorCode>, stopped: Option<CollectionStop> },
    BudgetExhausted,
    Unsupported,
    EvidenceUnavailable,
    Interrupted,
    Capacity,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticRecoveryProof {
    CompleteScope { capture: DiagnosticCaptureRef },
    FocusedParticipantGone { capture: DiagnosticCaptureRef, parent: Box<DiagnosticEvidenceRef> },
}
impl DiagnosticRecoveryProof {
    pub fn capture(&self) -> &DiagnosticCaptureRef {
        match self {
            Self::CompleteScope { capture } | Self::FocusedParticipantGone { capture, .. } => capture,
        }
    }
    pub fn matches_source(
        &self,
        episode: &DiagnosticEpisode,
        source: &CollectionDescription,
        capture: &DiagnosticCaptureRef,
    ) -> bool {
        if self.capture() != capture
            || !self.qualifies(&episode.first)
            || !episode.scope.is_valid()
            || source.target.telemetry_id().ok() != episode.scope.target.telemetry_id().ok()
            || source.captured_range().ok().map(|range| range.scope_id).as_deref()
                != Some(capture.source_scope_id.as_str())
        {
            return false;
        }
        match self {
            Self::CompleteScope { .. } => source.observation.activity().is_some_and(|snapshot| {
                snapshot.is_valid()
                    && ObservationBounds::new(&episode.last_seen_at, &snapshot.at).is_some()
                    && DiagnosticScope::from_source(source)
                        .is_ok_and(|scope| scope.key().ok() == episode.scope.key().ok())
                    && diagnostic_baseline_can_resolve(
                        &episode.scope,
                        snapshot.coverage.state,
                        snapshot.coverage.complete_scope,
                    )
            }),
            Self::FocusedParticipantGone { parent, .. } => match &source.observation {
                SourceObservation::FocusedPageWait { result, .. } => {
                    result.is_valid()
                        && ObservationBounds::new(&episode.last_seen_at, &result.observed_until).is_some()
                        && result.source == FocusSourceState::Matched
                        && result.request == PageWaitRequestState::Missing
                        && result.wait == PageWaitRelation::Unqualified
                        && &result.focus.participant().parent == parent.as_ref()
                        && episode.matches_page_wait_focus(&result.focus)
                }
                _ => false,
            },
        }
    }
    /// A shared result can establish the original participant's disappearance only through its admitted intent.
    /// The caller authenticates that intent and both original/owner receipts; a manual shared capture is insufficient.
    pub fn matches_source_with_intent(
        &self,
        episode: &DiagnosticEpisode,
        source: &CollectionDescription,
        capture: &DiagnosticCaptureRef,
        intent: &DiagnosticInvestigationIntent,
    ) -> bool {
        if !matches!(intent.execution, DiagnosticExecutionBinding::Shared { .. }) {
            return self.matches_source(episode, source, capture);
        }
        let Self::FocusedParticipantGone { parent, .. } = self else {
            return false;
        };
        self.capture() == capture
            && self.qualifies(&episode.first)
            && intent.matches_episode(episode)
            && intent.matches_result_source(source)
            && source.captured_range().ok().map(|range| range.scope_id).as_deref()
                == Some(capture.source_scope_id.as_str())
            && matches!(&source.observation, SourceObservation::FocusedPageWait { result, .. }
                if result.source == FocusSourceState::Matched
                    && result.request == PageWaitRequestState::Missing
                    && result.wait == PageWaitRelation::Unqualified
                    && ObservationBounds::new(&episode.last_seen_at, &result.observed_until).is_some()
                    && result.focus.participant().parent == **parent)
    }
    fn qualifies(&self, original: &DiagnosticEvidenceRef) -> bool {
        self.capture().is_valid()
            && match self {
                Self::CompleteScope { capture } => capture.source_scope_id == original.source_scope_id,
                Self::FocusedParticipantGone { parent, .. } => {
                    parent.is_valid()
                        && parent.source_scope_id == original.source_scope_id
                        && matches!(&parent.native_key, NativeRecordIdentity::Activity(_))
                        && parent.native_key == original.native_key
                }
            }
    }
}

/// The implemented native questions retain typed evidence; hashes are storage indexes, never diagnostic policy.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticSignal {
    Activity {
        key: ActivityKey,
        condition: ActivityCondition,
        policy: ActivityPolicy,
    },
    /// A native point observation; it establishes neither a blocking duration nor an owner.
    LockWait {
        key: ActivityKey,
    },
    RequestMemory {
        key: ActivityKey,
        statement_identity: Option<NativeIdentity>,
    },
    PageWait {
        key: ActivityKey,
        statement_identity: Option<NativeIdentity>,
        resource: crate::ObservedWaitResource,
    },
}
impl DiagnosticSignal {
    pub fn is_valid(&self) -> bool {
        self.activity_key().is_valid()
            && match self {
                Self::Activity { policy, .. } => policy.validate().is_ok(),
                Self::LockWait { key } => key.request.is_some(),
                Self::RequestMemory { key, statement_identity } => {
                    key.request.is_some() && statement_identity.as_ref().is_none_or(NativeIdentity::is_valid)
                }
                Self::PageWait { key, statement_identity, resource } => {
                    key.request.is_some()
                        && statement_identity.as_ref().is_none_or(NativeIdentity::is_valid)
                        && resource.is_valid()
                        && matches!(
                            resource.kind,
                            crate::WaitResourceKind::MemoryPage | crate::WaitResourceKind::PageIo
                        )
                }
            }
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        digest_value(&("diagnostic-signal-v1", self))
    }
    pub fn activity_key(&self) -> &ActivityKey {
        match self {
            Self::Activity { key, .. }
            | Self::LockWait { key }
            | Self::PageWait { key, .. }
            | Self::RequestMemory { key, .. } => key,
        }
    }
    fn matches_reference(&self, reference: &DiagnosticEvidenceRef) -> bool {
        matches!(&reference.native_key,NativeRecordIdentity::Activity(key) if key == self.activity_key())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticEpisode {
    pub id: String,
    pub scope: DiagnosticScope,
    pub signal: DiagnosticSignal,
    /// Exact decimal revision. A state transition cannot silently overwrite a previous revision.
    pub revision: String,
    pub lifecycle: DiagnosticLifecycle,
    pub analysis: DiagnosticAnalysisStatus,
    pub report: Option<DiagnosticReport>,
    pub issue: Option<DiagnosticAnalysisIssue>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub first: DiagnosticEvidenceRef,
    pub peak: Option<DiagnosticEvidenceRef>,
    pub latest: DiagnosticEvidenceRef,
    /// Recovery is a qualified complete capture; absence does not have a native row receipt.
    pub recovery: Option<DiagnosticRecoveryProof>,
    /// Compact reference only; the complete event belongs to immutable revision history.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub investigation_event: Option<String>,
}
impl DiagnosticEpisode {
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.id)
            && self.investigation_event.as_deref().is_none_or(valid_digest)
            && self.scope.is_valid()
            && self.signal.is_valid()
            && [&self.first, &self.latest]
                .into_iter()
                .chain(self.peak.iter())
                .all(|reference| self.signal.matches_reference(reference))
            && positive(&self.revision).is_some()
            && ObservationBounds::new(&self.first_seen_at, &self.last_seen_at).is_some()
            && [&self.first, &self.latest].into_iter().chain(self.peak.iter()).all(DiagnosticEvidenceRef::is_valid)
            && self.expected_id().is_ok_and(|expected| expected == self.id)
            && self.peak.as_ref().is_none_or(|peak| peak.source_scope_id == self.first.source_scope_id)
            && self.latest.source_scope_id == self.first.source_scope_id
            && (self.analysis != DiagnosticAnalysisStatus::Analyzed || self.report.is_some())
            && (!matches!(self.analysis, DiagnosticAnalysisStatus::Deferred | DiagnosticAnalysisStatus::Unresolved)
                || self.issue.is_some())
            && self.report.as_ref().is_none_or(|report| {
                report.is_valid()
                    && matches!(
                        (&self.signal, report),
                        (DiagnosticSignal::PageWait { .. }, DiagnosticReport::PageWait { .. })
                            | (DiagnosticSignal::RequestMemory { .. }, DiagnosticReport::RequestMemory { .. })
                    )
            })
            && match (&self.lifecycle, &self.recovery) {
                (DiagnosticLifecycle::Resolved, Some(proof)) => {
                    proof.qualifies(&self.first)
                        && self.scope.native_epoch.is_some()
                        && self.scope.partition.is_qualified()
                }
                (DiagnosticLifecycle::Active | DiagnosticLifecycle::Uncertain, None) => true,
                _ => false,
            }
    }
    pub fn start(
        scope: DiagnosticScope,
        signal: DiagnosticSignal,
        observed_at: String,
        evidence: DiagnosticEvidenceRef,
    ) -> Result<Self> {
        let id = digest_value(&("diagnostic-episode-v1", scope.key()?, &signal, evidence.key()?))?;
        let value = Self {
            id,
            scope,
            signal,
            revision: "1".into(),
            lifecycle: DiagnosticLifecycle::Active,
            analysis: DiagnosticAnalysisStatus::Pending,
            report: None,
            issue: None,
            first_seen_at: observed_at.clone(),
            last_seen_at: observed_at,
            first: evidence.clone(),
            peak: None,
            latest: evidence,
            recovery: None,
            investigation_event: None,
        };
        if !value.is_valid() {
            return Err(invalid());
        }
        Ok(value)
    }
    pub fn expected_id(&self) -> Result<String> {
        digest_value(&("diagnostic-episode-v1", self.scope.key()?, &self.signal, self.first.key()?))
    }
    /// Bind a focused question to this episode's native participant, statement, resource and original scope.
    /// The caller separately authenticates the focus's original row through its exact evidence receipt.
    pub fn matches_page_wait_focus(&self, focus: &ActivityWaitFocus) -> bool {
        self.is_valid()
            && focus.is_valid()
            && matches!(&self.signal, DiagnosticSignal::PageWait { key, statement_identity, resource }
                if *key == focus.participant().original.key
                    && statement_identity.as_ref() == Some(&focus.participant().statement_identity)
                    && *resource == focus.resource)
            && self.scope.source == focus.participant().source
            && self.scope.native_epoch.as_deref() == Some(focus.participant().epoch.as_str())
            && self.scope.partition == focus.participant().partition
            && self.scope.target.telemetry_id().ok() == focus.participant().target.telemetry_id().ok()
            && self.first.source_scope_id == focus.participant().parent.source_scope_id
            && ObservationBounds::new(&self.first_seen_at, &self.last_seen_at)
                .is_some_and(|bounds| bounds.contains(&focus.participant().original_observed_at))
    }
    pub fn matches_focus(&self, focus: &DiagnosticFocus) -> bool {
        match focus {
            DiagnosticFocus::PageWait { focus } => self.matches_page_wait_focus(focus),
            DiagnosticFocus::RequestMemory { focus } => self.matches_request_memory_focus(focus),
        }
    }
    pub fn matches_request_memory_focus(&self, focus: &ActivityRequestFocus) -> bool {
        self.is_valid()
            && focus.is_request_memory_focus()
            && matches!(&self.signal, DiagnosticSignal::RequestMemory { key, statement_identity }
                if *key == focus.original.key && statement_identity.as_ref() == Some(&focus.statement_identity))
            && self.scope.source == focus.source
            && self.scope.native_epoch.as_deref() == Some(focus.epoch.as_str())
            && self.scope.partition == focus.partition
            && self.scope.target.telemetry_id().ok() == focus.target.telemetry_id().ok()
            && self.first.source_scope_id == focus.parent.source_scope_id
            && ObservationBounds::new(&self.first_seen_at, &self.last_seen_at)
                .is_some_and(|bounds| bounds.contains(&focus.original_observed_at))
    }
    pub fn retained_in_checkpoint(&self) -> bool {
        self.lifecycle != DiagnosticLifecycle::Resolved || self.analysis.needs_work()
    }
}

/// A typed reference to a durable result keeps rendering language-neutral and avoids copying native evidence.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticReport {
    PageWait { observation: DiagnosticEvidenceRef, blocking_contexts: Vec<DiagnosticEvidenceRef> },
    RequestMemory { observation: DiagnosticEvidenceRef, peers: Vec<DiagnosticEvidenceRef> },
}
impl DiagnosticReport {
    pub fn matches_observation(
        &self,
        episode: &DiagnosticEpisode,
        source: &CollectionDescription,
        row: &ExportedRecord,
    ) -> bool {
        match (self, &source.observation, &row.record) {
            (
                Self::PageWait { .. },
                SourceObservation::FocusedPageWait { result, .. },
                TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }),
            ) => {
                episode.matches_page_wait_focus(&result.focus)
                    && result.focus.matches_capture_scope(source)
                    && result.accepts_observation(page)
            }
            (
                Self::RequestMemory { .. },
                SourceObservation::FocusedRequestMemory { result, .. },
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(grant),
                    ..
                }),
            ) => {
                episode.matches_request_memory_focus(&result.focus)
                    && result.focus.matches_capture_scope(source)
                    && result.is_subject(grant)
            }
            _ => false,
        }
    }
    pub fn evidence(&self) -> &DiagnosticEvidenceRef {
        match self {
            Self::PageWait { observation, .. } | Self::RequestMemory { observation, .. } => observation,
        }
    }
    /// Every raw row used by this explanation participates in pinning, validation and immutable history.
    pub fn references(&self) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
        let secondary = match self {
            Self::PageWait { blocking_contexts, .. } => blocking_contexts,
            Self::RequestMemory { peers, .. } => peers,
        };
        std::iter::once(self.evidence()).chain(secondary)
    }
    pub fn is_valid(&self) -> bool {
        let mut unique = BTreeSet::new();
        self.references().count() <= MAX_DIAGNOSTIC_INPUTS
            && self.references().all(|reference| reference.key().is_ok_and(|key| unique.insert(key)))
            && matches!(self.evidence().native_key, NativeRecordIdentity::Operational { .. })
            && match self {
                Self::PageWait { blocking_contexts, .. } => {
                    blocking_contexts.len() <= crate::page_wait_report::MAX_PAGE_WAIT_BLOCKING_CONTEXTS
                        && blocking_contexts
                            .iter()
                            .all(|reference| reference.stream == TelemetryStream::ActivityObservations)
                }
                Self::RequestMemory { peers, .. } => peers.iter().all(|reference| {
                    reference.capture_ref() == self.evidence().capture_ref()
                        && matches!(reference.native_key, NativeRecordIdentity::Operational { .. })
                }),
            }
    }

    /// A focused grant report retains every declared raw participant from the same authenticated input.
    pub fn matches_request_memory_input<'a>(
        &self,
        source: &CollectionDescription,
        rows: impl IntoIterator<Item = (&'a DiagnosticEvidenceRef, &'a ExecutionGrantObservation)>,
    ) -> bool {
        let Self::RequestMemory { observation, peers } = self else { return false };
        let SourceObservation::FocusedRequestMemory { result, .. } = &source.observation else { return false };
        let rows = rows.into_iter().collect::<Vec<_>>();
        let mut references = BTreeSet::new();
        self.is_valid()
            && rows.len() == peers.len() + 1
            && result.matches_observations(rows.iter().map(|(_, row)| *row))
            && rows.iter().all(|(reference, row)| {
                reference.key().is_ok_and(|key| references.insert(key))
                    && if result.is_subject(row) { *reference == observation } else { peers.contains(reference) }
            })
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticRevision {
    pub id: String,
    pub controller_version: u32,
    pub episode: DiagnosticEpisode,
    pub previous_revision: Option<String>,
    pub inputs: Vec<DiagnosticEvidenceRef>,
    pub analyzed_at: String,
    /// Present only when this transition installs a new immutable investigation event.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub investigation: Option<Box<DiagnosticInvestigationEvent>>,
}
impl DiagnosticRevision {
    pub fn is_valid(&self) -> bool {
        let revision = positive(&self.episode.revision);
        let previous = self.previous_revision.as_deref().and_then(positive);
        let ordered = match (revision, previous, &self.previous_revision) {
            (Some(1), None, None) => true,
            (Some(now), Some(before), Some(_)) => before.checked_add(1) == Some(now),
            _ => false,
        };
        let mut unique = BTreeSet::new();
        self.controller_version > 0
            && self.episode.is_valid()
            && ordered
            && valid_utc_instant(&self.analyzed_at)
            && !self.inputs.is_empty()
            && self.inputs.len() <= MAX_DIAGNOSTIC_INPUTS
            && self.inputs.iter().all(|reference| reference.key().is_ok_and(|key| unique.insert(key)))
            && self.investigation.as_ref().is_none_or(|event| {
                event.matches_episode(&self.episode)
                    && event.key().ok().as_deref() == self.episode.investigation_event.as_deref()
                    && self.inputs.contains(&event.intent.focus.participant().parent)
                    && match &event.intent.execution {
                        DiagnosticExecutionBinding::Database => true,
                        DiagnosticExecutionBinding::Shared { owner } => self.inputs.contains(owner),
                    }
            })
            && (self.previous_revision.is_some()
                || self.investigation.is_some()
                || self.episode.investigation_event.is_none())
            && (self.episode.analysis != DiagnosticAnalysisStatus::Analyzed || self.episode.report.is_some())
            && self.episode.report.as_ref().is_none_or(|report| {
                report.is_valid() && report.references().all(|reference| self.inputs.contains(reference))
            })
            && self.expected_id().is_ok_and(|expected| expected == self.id)
    }
    pub fn expected_id(&self) -> Result<String> {
        digest_value(&(
            "diagnostic-revision-v1",
            self.controller_version,
            &self.episode,
            &self.previous_revision,
            &self.inputs,
            &self.investigation,
        ))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticCheckpoint {
    pub controller_version: u32,
    pub target: CollectionTarget,
    pub input: DiagnosticCaptureRef,
    pub episodes: Vec<DiagnosticEpisode>,
}
impl DiagnosticCheckpoint {
    pub fn is_valid(&self) -> bool {
        let mut ids = BTreeSet::new();
        self.controller_version > 0
            && self.target.is_valid()
            && self.input.is_valid()
            && self.episodes.len() <= MAX_DIAGNOSTIC_EPISODES
            && self.episodes.iter().all(|episode| {
                episode.is_valid()
                    && episode.retained_in_checkpoint()
                    && episode.scope.target.telemetry_id().ok() == self.target.telemetry_id().ok()
                    && ids.insert(&episode.id)
            })
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        digest_value(&("diagnostic-checkpoint-v1", self.target.telemetry_id()?, self.controller_version))
    }
}

/// Bounded recovery-only checkpoint rows; assembly must establish the complete working set before state restoration.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticCheckpointPart {
    pub id: String,
    pub controller_version: u32,
    pub target: CollectionTarget,
    pub input: DiagnosticCaptureRef,
    pub part: u32,
    pub parts: u32,
    pub episodes: Vec<DiagnosticEpisode>,
}
impl DiagnosticCheckpointPart {
    pub fn is_valid(&self) -> bool {
        self.expected_id().is_ok_and(|id| id == self.id)
            && self.parts > 0
            && self.parts <= MAX_DIAGNOSTIC_EPISODES as u32
            && self.part < self.parts
            && (self.parts == 1 || !self.episodes.is_empty())
            && DiagnosticCheckpoint {
                controller_version: self.controller_version,
                target: self.target.clone(),
                input: self.input.clone(),
                episodes: self.episodes.clone(),
            }
            .is_valid()
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        Ok(self.id.clone())
    }
    pub fn expected_id(&self) -> Result<String> {
        digest_value(&(
            "diagnostic-checkpoint-part-v1",
            self.target.telemetry_id()?,
            self.controller_version,
            self.part,
        ))
    }
    pub fn assemble(mut parts: Vec<Self>) -> Result<DiagnosticCheckpoint> {
        if parts.is_empty() || parts.len() > MAX_DIAGNOSTIC_EPISODES {
            return Err(invalid());
        }
        parts.sort_by_key(|part| part.part);
        let first = parts.first().ok_or_else(invalid)?;
        let mut checkpoint = DiagnosticCheckpoint {
            controller_version: first.controller_version,
            target: first.target.clone(),
            input: first.input.clone(),
            episodes: Vec::new(),
        };
        let count = parts.len() as u32;
        for (index, part) in parts.into_iter().enumerate() {
            if !part.is_valid()
                || part.part != index as u32
                || part.parts != count
                || part.controller_version != checkpoint.controller_version
                || part.target != checkpoint.target
                || part.input != checkpoint.input
            {
                return Err(invalid());
            }
            checkpoint.episodes.extend(part.episodes);
            if checkpoint.episodes.len() > MAX_DIAGNOSTIC_EPISODES {
                return Err(invalid());
            }
        }
        if !checkpoint.is_valid() {
            return Err(invalid());
        }
        Ok(checkpoint)
    }
}

/// Only positive evidence is admitted from partial input; unknown visibility cannot clear older episodes.
pub fn diagnostic_baseline_can_resolve(
    scope: &DiagnosticScope,
    source_state: SourceState,
    complete_scope: bool,
) -> bool {
    scope.is_valid()
        && scope.native_epoch.is_some()
        && scope.partition.is_qualified()
        && complete_scope
        && matches!(source_state, SourceState::Collected | SourceState::Empty)
}

pub(super) fn valid_capture(capture: &CaptureIdentity) -> bool {
    valid_key(&capture.observation_id)
        && capture.revision.as_ref().is_some_and(|revision| {
            valid_key(&revision.outbox_id)
                && positive(&revision.owner_epoch).is_some()
                && positive(&revision.sequence).is_some()
        })
}
pub(super) fn positive(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|parsed| *parsed > 0 && parsed.to_string() == value)
}
pub(super) fn digest_value(value: &impl Serialize) -> Result<String> {
    serde_json::to_vec(value).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
