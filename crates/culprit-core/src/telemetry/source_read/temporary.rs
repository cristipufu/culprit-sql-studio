//! A finite resource question preserves independently clocked originals. Allocation counters and an old
//! version-protection boundary are context, never an exclusive explanation of tempdb pressure.
use super::*;
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TemporaryResourceMechanism {
    UserAllocations,
    InternalAllocations,
    CleanupBoundary,
    VersionProtection,
    VersionGeneration,
    SpillOrEstimate,
    ConcurrentReservations,
    DataCapacity,
    LogCapacity,
    AllocationLatch,
    PhysicalIo,
}
impl TemporaryResourceMechanism {
    pub const ALL: &'static [Self] = &[
        Self::UserAllocations,
        Self::InternalAllocations,
        Self::CleanupBoundary,
        Self::VersionProtection,
        Self::VersionGeneration,
        Self::SpillOrEstimate,
        Self::ConcurrentReservations,
        Self::DataCapacity,
        Self::LogCapacity,
        Self::AllocationLatch,
        Self::PhysicalIo,
    ];
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum TemporaryResourceContribution {
    /// Original native facts are present. This says neither that they are harmful nor that they caused a wait.
    Observed {
        mechanism: TemporaryResourceMechanism,
        observations: u32,
        /// Bounded exact receipt prefix; all original rows remain independently retained.
        evidence: Vec<DiagnosticEvidenceRef>,
    },
    Unresolved {
        mechanism: TemporaryResourceMechanism,
    },
}
impl TemporaryResourceContribution {
    pub fn mechanism(&self) -> TemporaryResourceMechanism {
        match self {
            Self::Observed { mechanism, .. } | Self::Unresolved { mechanism } => *mechanism,
        }
    }
    pub fn evidence(&self) -> &[DiagnosticEvidenceRef] {
        match self {
            Self::Observed { evidence, .. } => evidence,
            Self::Unresolved { .. } => &[],
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TemporaryResourceAnswer {
    pub basis: DiagnosticEvidenceRef,
    pub basis_context: NativeReadContext,
    pub result: DiagnosticCaptureRef,
    pub result_context: NativeReadContext,
    /// Native source coverage, independent of how many competing mechanisms remain unresolved.
    pub source_state: crate::SourceState,
    pub contributions: Vec<TemporaryResourceContribution>,
    /// Only these exact retained task rows have checked request/statement generations. Other allocation
    /// counters remain useful native context without an allocating-query or transaction attribution.
    pub observed_tasks: u32,
    pub checked_task_context_count: u32,
    pub checked_task_contexts: Vec<DiagnosticEvidenceRef>,
}
impl TemporaryResourceAnswer {
    pub fn from_originals(
        focus: &SourceReadFocus,
        original_source: &CollectionDescription,
        original: &ExportedRecord,
        source: &CollectionDescription,
        capture: &DiagnosticCaptureRef,
        rows: &[(&DiagnosticEvidenceRef, &ExportedRecord)],
    ) -> Result<Self> {
        let basis_context = focus.original_context(original_source)?;
        let execution = SourceReadExecutionScope::from_context(
            focus.question,
            source.observation.execution_context().ok_or_else(invalid)?,
        )?;
        let SourceObservation::SourceRead { snapshot, focus_key, execution: result_context } = &source.observation
        else {
            return Err(invalid());
        };
        if focus.question != SourceReadQuestion::TemporarySpaceContributors
            || !focus.matches_basis(original_source, original, &execution)
            || !source.partition_is_consistent()
            || !source.observation.covers_request()
            || focus.key()? != *focus_key
            || !execution.matches_context(result_context)
            || !capture.is_valid()
            || capture.capture == focus.basis.capture
            || source.captured_range()?.scope_id != capture.source_scope_id
            || snapshot.records as usize != rows.len()
            || rows.len() > MAX_TEMPORARY_ANSWER_TASKS
            || ObservationBounds::new(&basis_context.observed_until, &result_context.observed_from).is_none()
        {
            return Err(invalid());
        }
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlCleanupHorizon(boundary),
            ..
        }) = &original.record
        else {
            return Err(invalid());
        };
        // A logged transaction alone does not establish a row-version protector. Preserve the explicit native
        // snapshot/XSN evidence separately; even that context does not attribute particular retained bytes.
        let observed_snapshot = boundary
            .oldest_confirmed
            .as_deref()
            .is_some_and(|row| row.row_version_participant && row.snapshot_isolation == Some(true));
        let tasks = validated_tasks(source, capture, rows)?;
        let refused = ActivityEvidence::observed_session_generation_refusals(
            tasks.iter().filter_map(|(_, row)| row.request.as_ref()),
        )?;
        let checked_requests = tasks
            .iter()
            .filter_map(|(reference, row)| {
                let request = row.request.as_ref()?;
                let statement = row.statement_identity.as_ref()?;
                (row.request_context() == TaskRequestContext::CheckedAtBounds
                    && request.has_session_generation_authority()
                    && !refused.contains(&request.key.session)
                    && request.statement_identity.as_ref() == Some(statement)
                    && result_context.request_key(&request.key, statement).is_some())
                .then(|| (*reference).clone())
            })
            .collect::<Vec<_>>();
        let contributions = TemporaryResourceMechanism::ALL
            .iter()
            .map(|mechanism| {
                let evidence = match mechanism {
                    TemporaryResourceMechanism::CleanupBoundary => vec![focus.basis.as_ref().clone()],
                    TemporaryResourceMechanism::VersionProtection if observed_snapshot => {
                        vec![focus.basis.as_ref().clone()]
                    }
                    TemporaryResourceMechanism::UserAllocations => {
                        allocation_evidence(&tasks, |row| &row.user_allocated_pages)
                    }
                    TemporaryResourceMechanism::InternalAllocations => {
                        allocation_evidence(&tasks, |row| &row.internal_allocated_pages)
                    }
                    _ => vec![],
                };
                if evidence.is_empty() {
                    TemporaryResourceContribution::Unresolved { mechanism: *mechanism }
                } else {
                    TemporaryResourceContribution::Observed {
                        mechanism: *mechanism,
                        observations: evidence.len() as u32,
                        evidence: evidence.into_iter().take(MAX_TEMPORARY_EVIDENCE_PER_MECHANISM).collect(),
                    }
                }
            })
            .collect();
        let answer = Self {
            basis: focus.basis.as_ref().clone(),
            basis_context,
            result: capture.clone(),
            result_context: result_context.as_ref().clone(),
            source_state: snapshot.coverage_state()?,
            contributions,
            observed_tasks: tasks.len() as u32,
            checked_task_context_count: checked_requests.len() as u32,
            checked_task_contexts: checked_requests.into_iter().take(MAX_TEMPORARY_CHECKED_CONTEXTS).collect(),
        };
        if !answer.is_valid() {
            return Err(invalid());
        }
        Ok(answer)
    }
    pub fn is_valid(&self) -> bool {
        let Ok(execution) = LocalResourceReadScope::from_context(&self.result_context) else { return false };
        self.basis.is_valid()
            && self.basis.stream == TelemetryStream::OperationalSamples
            && self.result.is_valid()
            && self.result.capture != self.basis.capture
            && self.result.source_scope_id != self.basis.source_scope_id
            && execution.matches_original_context(&self.basis_context)
            && ObservationBounds::new(&self.basis_context.observed_until, &self.result_context.observed_from).is_some()
            && self.observed_tasks as usize <= MAX_TEMPORARY_ANSWER_TASKS
            && self.source_population_is_consistent()
            && self.contributions_are_valid()
            && self.checked_contexts_are_valid()
    }
    fn source_population_is_consistent(&self) -> bool {
        match self.source_state {
            crate::SourceState::Collected => self.observed_tasks > 0,
            crate::SourceState::Empty | crate::SourceState::Unavailable | crate::SourceState::NotApplicable => {
                self.observed_tasks == 0
            }
            crate::SourceState::Partial => true,
        }
    }
    fn contributions_are_valid(&self) -> bool {
        let mut kinds = BTreeSet::new();
        self.contributions.len() == TemporaryResourceMechanism::ALL.len()
            && self
                .contributions
                .iter()
                .all(|contribution| kinds.insert(contribution.mechanism()) && contribution.is_valid_for(self))
    }
    fn checked_contexts_are_valid(&self) -> bool {
        let mut references = BTreeSet::new();
        self.checked_task_context_count <= self.observed_tasks
            && self.checked_task_contexts.len()
                == (self.checked_task_context_count as usize).min(MAX_TEMPORARY_CHECKED_CONTEXTS)
            && self.checked_task_contexts.iter().all(|reference| {
                reference.is_valid()
                    && reference.capture_ref() == self.result
                    && reference.key().is_ok_and(|key| references.insert(key))
            })
    }
    pub fn references(&self) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
        std::iter::once(&self.basis)
            .chain(self.contributions.iter().flat_map(TemporaryResourceContribution::evidence))
            .chain(&self.checked_task_contexts)
    }
}

impl TemporaryResourceContribution {
    fn is_valid_for(&self, answer: &TemporaryResourceAnswer) -> bool {
        let Self::Observed { mechanism, observations, evidence } = self else {
            return self.mechanism() != TemporaryResourceMechanism::CleanupBoundary;
        };
        let boundary = matches!(
            mechanism,
            TemporaryResourceMechanism::CleanupBoundary | TemporaryResourceMechanism::VersionProtection
        );
        let allowed = boundary
            || matches!(
                mechanism,
                TemporaryResourceMechanism::UserAllocations | TemporaryResourceMechanism::InternalAllocations
            );
        let maximum = if boundary { 1 } else { answer.observed_tasks };
        let mut references = BTreeSet::new();
        allowed
            && *observations > 0
            && *observations <= maximum
            && evidence.len() == (*observations as usize).min(MAX_TEMPORARY_EVIDENCE_PER_MECHANISM)
            && evidence.iter().all(|reference| {
                reference.is_valid()
                    && if boundary { reference == &answer.basis } else { reference.capture_ref() == answer.result }
            })
            && evidence.iter().all(|reference| reference.key().is_ok_and(|key| references.insert(key)))
    }
}

const MAX_TEMPORARY_ANSWER_TASKS: usize = 200;
const MAX_TEMPORARY_EVIDENCE_PER_MECHANISM: usize = 4;
const MAX_TEMPORARY_CHECKED_CONTEXTS: usize = 8;

fn validated_tasks<'a>(
    source: &CollectionDescription,
    capture: &DiagnosticCaptureRef,
    rows: &'a [(&'a DiagnosticEvidenceRef, &'a ExportedRecord)],
) -> Result<Vec<(&'a DiagnosticEvidenceRef, &'a TaskSpaceObservation)>> {
    let mut keys = BTreeSet::new();
    rows.iter()
        .enumerate()
        .map(|(ordinal, (reference, row))| {
            reference.validate_canonical_record(source, &row.record)?;
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::TaskSpace(task), .. }) =
                &row.record
            else {
                return Err(invalid());
            };
            if reference.capture_ref() != *capture
                || reference.capture != row.capture
                || reference.row_ordinal != ordinal as u32
                || reference.batch_id != row.batch_id
                || reference.payload_sha256 != row.payload_sha256()?
                || !keys.insert(&task.task)
            {
                return Err(invalid());
            }
            Ok((*reference, task.as_ref()))
        })
        .collect()
}
fn allocation_evidence(
    rows: &[(&DiagnosticEvidenceRef, &TaskSpaceObservation)],
    select: fn(&TaskSpaceObservation) -> &Option<String>,
) -> Vec<DiagnosticEvidenceRef> {
    rows.iter()
        .filter(|(_, row)| {
            select(row).as_deref().and_then(|value| value.parse::<u64>().ok()).is_some_and(|value| value > 0)
        })
        .map(|(reference, _)| (*reference).clone())
        .collect()
}

impl DiagnosticEpisode {
    pub fn complete_temporary_resource_read(
        &self,
        intent: &DiagnosticInvestigationIntent,
        original_source: &CollectionDescription,
        original: &ExportedRecord,
        source: &CollectionDescription,
        capture: &DiagnosticCaptureRef,
        rows: &[(&DiagnosticEvidenceRef, &ExportedRecord)],
    ) -> Result<Self> {
        let DiagnosticFocus::SourceRead { focus } = &intent.focus else { return Err(invalid()) };
        if !self.is_valid()
            || !self.needs_question(&intent.focus)
            || !intent.matches_episode(self)
            || !intent.matches_result_source(source)
        {
            return Err(invalid());
        }
        let answer = TemporaryResourceAnswer::from_originals(focus, original_source, original, source, capture, rows)?;
        self.transition_source_read(SourceReadStageState::Answered {
            answer: Box::new(SourceReadAnswer::TemporaryResources(Box::new(answer))),
        })
    }
}
