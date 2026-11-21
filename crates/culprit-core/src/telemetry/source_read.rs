//! Finite source questions keep logical data identity separate from the qualified execution/cost owner.
use super::*;

mod scope;
pub use scope::*;
mod answer;
pub use answer::*;
mod temporary;
pub use temporary::*;
mod query_compilation;
pub use query_compilation::*;

/// Native updateability qualifies this exact execution expectation. It names neither a replica role nor a
/// query/execution population, and cannot supply a member or process identity that the source did not report.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NativeSourceReadScope {
    pub target: crate::DatabaseIdentity,
    pub partition: SourcePartition,
    pub database_writable: bool,
}
impl NativeSourceReadScope {
    pub fn from_context(context: &NativeReadContext) -> Result<Self> {
        let CollectionTarget::Database(target) = &context.target else { return Err(invalid()) };
        let value = Self {
            target: target.clone(),
            partition: context.partition.clone(),
            database_writable: context.database_writable.ok_or_else(invalid)?,
        };
        if !context.is_valid() || !value.is_valid() {
            return Err(invalid());
        }
        Ok(value)
    }
    pub fn is_valid(&self) -> bool {
        let target = CollectionTarget::Database(self.target.clone());
        target.is_valid()
            && self.target.incarnation.as_ref().is_some_and(|identity| !identity.is_empty())
            && self.partition.is_valid_for(&target)
            && matches!(&self.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) })
            && self.partition.is_qualified()
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("native-source-read-scope-v1", self.target.telemetry_id()?, &self.partition, self.database_writable))
    }
    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        self.is_valid() && Self::from_context(context).is_ok_and(|actual| actual.key().ok() == self.key().ok())
    }
    pub fn matches_target(&self, target: &CollectionTarget) -> bool {
        self.is_valid() && target.database().is_some() && target.telemetry_id().ok() == self.target.telemetry_id().ok()
    }
}

/// Only implemented questions can acquire automatic source work. Native providers declare supported routes;
/// engine identity and a generic Snapshot selector confer no additional admission or counter semantics.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceReadQuestion {
    CompilationHistory,
    HyperscaleLogComponents,
    TemporarySpaceContributors,
}
impl SourceReadQuestion {
    pub const ALL: &'static [Self] =
        &[Self::CompilationHistory, Self::HyperscaleLogComponents, Self::TemporarySpaceContributors];

    pub const fn dataset(self) -> SnapshotDataset {
        match self {
            Self::CompilationHistory => SnapshotDataset::QueryCompilation,
            Self::HyperscaleLogComponents => SnapshotDataset::HyperscaleLogComponents,
            Self::TemporarySpaceContributors => SnapshotDataset::TaskSpace,
        }
    }
    pub const fn selector(self) -> RecipeSelector {
        RecipeSelector::Collection(TelemetryCollection::Snapshot(self.dataset()))
    }
    pub fn accepts_dataset(self, dataset: SnapshotDataset) -> bool {
        self.dataset() == dataset
    }
    pub fn for_dataset(dataset: SnapshotDataset) -> Option<Self> {
        Self::ALL.iter().copied().find(|question| question.accepts_dataset(dataset))
    }
}

/// Only compilation history requires a prior gateway answer. A native log-governance wait is itself the
/// original incident evidence; it cannot borrow or manufacture a compilation prerequisite.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum SourceReadPrerequisite {
    CompilationAdmission { report: Box<CompilationAdmissionReport> },
    ObservedLogRateWait,
    ObservedCleanupBoundary { session: NativeActivityIdentity, transaction: NativeActivityIdentity },
    ObservedQueryHistoryFailure { query: NativeQueryIdentity },
}
impl SourceReadPrerequisite {
    pub fn references(&self) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
        self.compilation_admission().into_iter().flat_map(CompilationAdmissionReport::references)
    }

    pub fn compilation_admission(&self) -> Option<&CompilationAdmissionReport> {
        match self {
            Self::CompilationAdmission { report } => Some(report),
            Self::ObservedLogRateWait
            | Self::ObservedCleanupBoundary { .. }
            | Self::ObservedQueryHistoryFailure { .. } => None,
        }
    }
    fn matches_cleanup_condition(&self, condition: &DiagnosticOriginalCondition) -> bool {
        match (self, condition) {
            (
                Self::ObservedCleanupBoundary { session, transaction },
                DiagnosticOriginalCondition::MssqlCleanupBoundary {
                    session: actual_session,
                    transaction: actual_transaction,
                },
            ) => session == actual_session && transaction == actual_transaction,
            _ => false,
        }
    }
    fn is_valid_for(&self, question: SourceReadQuestion) -> bool {
        match (self, question) {
            (Self::CompilationAdmission { report }, SourceReadQuestion::CompilationHistory) => report.is_valid(),
            (Self::ObservedQueryHistoryFailure { query }, SourceReadQuestion::CompilationHistory) => {
                query_store_query_identity_is_complete(query)
            }
            (Self::ObservedLogRateWait, SourceReadQuestion::HyperscaleLogComponents) => true,
            (
                Self::ObservedCleanupBoundary { session, transaction },
                SourceReadQuestion::TemporarySpaceContributors,
            ) => {
                session.is_valid()
                    && transaction.is_valid()
                    && !session.generation.trim().is_empty()
                    && !transaction.generation.trim().is_empty()
            }
            _ => false,
        }
    }
}

/// An exact genuine incident receipt justifies one finite context question. No Activity/request/SQL
/// envelope is invented for source-global evidence. The caller authenticates the original independently.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SourceReadFocus {
    pub basis: Box<DiagnosticEvidenceRef>,
    /// Logical dataset namespace. An admitted native execution partition never replaces it.
    pub target: CollectionTarget,
    pub question: SourceReadQuestion,
    pub prerequisite: SourceReadPrerequisite,
}
impl SourceReadFocus {
    pub fn is_valid(&self) -> bool {
        self.basis.is_valid()
            && self.prerequisite.is_valid_for(self.question)
            && match (&self.prerequisite, self.question) {
                (
                    SourceReadPrerequisite::ObservedQueryHistoryFailure { .. },
                    SourceReadQuestion::CompilationHistory,
                ) => {
                    self.basis.stream == TelemetryStream::QueryRuntime
                        && matches!(self.basis.native_key, NativeRecordIdentity::Runtime(_))
                }
                (_, SourceReadQuestion::TemporarySpaceContributors) => {
                    self.basis.stream == TelemetryStream::OperationalSamples
                        && matches!(self.basis.native_key, NativeRecordIdentity::Operational { .. })
                }
                _ => {
                    self.basis.stream == TelemetryStream::ActivityObservations
                        && matches!(self.basis.native_key, NativeRecordIdentity::Activity(_))
                }
            }
            && self.target.is_valid()
            && self.target.database().is_some()
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("source-read-focus-v1", &self.basis, self.target.telemetry_id()?, self.question, &self.prerequisite))
    }
    /// Consistency over separately caller-authenticated original bodies. The existing native request key
    /// compares full source/request/statement ownership, without using display SQL, IDs alone or poll clocks.
    pub fn matches_prerequisite_input<'a>(
        &self,
        original_source: &CollectionDescription,
        original_row: &ExportedRecord,
        source: &CollectionDescription,
        rows: impl IntoIterator<Item = (&'a DiagnosticEvidenceRef, &'a OperationalObservation)>,
    ) -> bool {
        let Some(prerequisite) = self.prerequisite.compilation_admission() else { return false };
        let (
            Ok(original),
            SourceObservation::FocusedCompilationAdmission { result, .. },
            TelemetryRecord::Activity(actor),
        ) = (NativeReadContext::from_activity(original_source), &source.observation, &original_row.record)
        else {
            return false;
        };
        let Ok(execution) = SourceReadExecutionScope::from_context(self.question, &original) else { return false };
        let Some(statement) = &actor.statement_identity else { return false };
        let participant = &result.focus;
        let context = NativeReadContext {
            target: participant.target.clone(),
            partition: participant.partition.clone(),
            observed_from: participant.original_observed_at.clone(),
            observed_until: participant.original_observed_at.clone(),
            database_writable: participant.database_writable,
        };
        self.matches_basis(original_source, original_row, &execution)
            && participant.source == original_source.source
            && original.partition.native_epoch() == Some(participant.epoch.as_str())
            && original
                .request_key(&actor.key, statement)
                .zip(context.request_key(&participant.original.key, &participant.statement_identity))
                .is_some_and(|(left, right)| left == right)
            && prerequisite.matches_input(source, rows)
    }
    /// Only closed, bounded answer families materialize their original rows for offline diagnosis.
    pub fn retains_result_rows(&self) -> bool {
        matches!(
            self.prerequisite,
            SourceReadPrerequisite::ObservedCleanupBoundary { .. }
                | SourceReadPrerequisite::ObservedQueryHistoryFailure { .. }
        )
    }
    /// The existing canonical reader verifies every original row and byte before retaining this selection.
    /// Query-number candidates include changed incarnations, which the answer must explicitly refuse.
    pub fn retains_result_record(&self, record: &TelemetryRecord) -> bool {
        match &self.prerequisite {
            SourceReadPrerequisite::ObservedQueryHistoryFailure { query } => match record {
                TelemetryRecord::Operational(OperationalObservation {
                    source,
                    fact: OperationalFact::MssqlQueryCompilation(point),
                    ..
                }) if source == crate::source_id::QUERY_COMPILATION => point.identity.query_id == query.query_id,
                // Omit only known unrelated queries. A foreign family/source survives selection for refusal.
                _ => true,
            },
            _ => self.retains_result_rows(),
        }
    }
    pub fn selects_result_rows(&self) -> bool {
        matches!(self.prerequisite, SourceReadPrerequisite::ObservedQueryHistoryFailure { .. })
    }
    pub fn original_context(&self, source: &CollectionDescription) -> Result<NativeReadContext> {
        if matches!(self.prerequisite, SourceReadPrerequisite::ObservedQueryHistoryFailure { .. }) {
            return SourceReadExecutionScope::context_from_source(source);
        }
        match self.question {
            SourceReadQuestion::TemporarySpaceContributors => NativeReadContext::from_snapshot(source),
            _ => NativeReadContext::from_activity(source),
        }
    }
    fn original_condition_matches(&self, source: &CollectionDescription, row: &TelemetryRecord) -> bool {
        if let SourceReadPrerequisite::ObservedQueryHistoryFailure { query } = &self.prerequisite {
            let Some(DiagnosticOriginalCondition::MssqlQueryHistory { query: original, .. }) =
                DiagnosticOriginalCondition::from_record(source, row)
            else {
                return false;
            };
            return query_store_query_incarnation_key(query).ok()
                == query_store_query_incarnation_key(&NativeQueryIdentity {
                    query_id: original.id,
                    qualifiers: original.qualifiers,
                })
                .ok();
        }
        match self.question {
            SourceReadQuestion::TemporarySpaceContributors => {
                let Some(condition) = DiagnosticOriginalCondition::from_record(source, row) else {
                    return false;
                };
                self.prerequisite.matches_cleanup_condition(&condition) && cleanup_attention(row)
            }
            question => matches!(row, TelemetryRecord::Activity(original)
            if original.has_session_generation_authority()
                && original.scope == ActivityScopeBasis::RequestDatabase
                && original.wait.as_ref().is_some_and(|wait| wait.class == Some(match question {
                    SourceReadQuestion::CompilationHistory => ActivityWaitClass::CompilationAdmission,
                    SourceReadQuestion::HyperscaleLogComponents => ActivityWaitClass::LogRateGovernance,
                    SourceReadQuestion::TemporarySpaceContributors => return false,
                }))),
        }
    }
    /// Authenticate the exact retained prerequisite independently of the new read's native admission.
    pub fn matches_original(&self, source: &CollectionDescription, row: &ExportedRecord) -> bool {
        self.is_valid()
            && source.target.telemetry_id().ok() == self.target.telemetry_id().ok()
            && self.original_condition_matches(source, &row.record)
            && self.basis.validate_canonical_record(source, &row.record).is_ok()
            && self.basis.capture == row.capture
            && row.batch_id == self.basis.batch_id
            && row.payload_sha256().is_ok_and(|digest| digest == self.basis.payload_sha256)
    }
    pub fn matches_basis(
        &self,
        source: &CollectionDescription,
        row: &ExportedRecord,
        execution: &SourceReadExecutionScope,
    ) -> bool {
        self.matches_original(source, row)
            && execution.matches_target(&self.target)
            && execution.accepts_question(self.question)
            && self.original_context(source).is_ok_and(|context| execution.matches_original_context(&context))
    }
}

/// The completed waiter/gateway answer stays immutable. This separately owed question transitions only through
/// revision/event history, and retains one successful capture rather than a per-row compiler roster.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SourceReadStage {
    pub focus: SourceReadFocus,
    pub state: SourceReadStageState,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum SourceReadStageState {
    Pending,
    Deferred { issue: DiagnosticAnalysisIssue },
    Collecting,
    Captured { capture: DiagnosticCaptureRef },
    Answered { answer: Box<SourceReadAnswer> },
    Recovered,
    Stopped { issue: DiagnosticAnalysisIssue },
}
impl SourceReadStage {
    pub fn is_valid(&self) -> bool {
        self.focus.is_valid()
            && match &self.state {
                SourceReadStageState::Answered { answer } => {
                    self.focus.question == answer.question()
                        && answer.is_valid()
                        && answer.basis() == self.focus.basis.as_ref()
                }
                SourceReadStageState::Captured { capture } => {
                    !self.focus.retains_result_rows()
                        && capture.is_valid()
                        && capture.capture != self.focus.basis.capture
                        && capture.source_scope_id != self.focus.basis.source_scope_id
                }
                _ => true,
            }
    }
    /// Stable finite-stage identity can be checked before recovering original bodies or cost artifacts.
    pub fn key(&self, episode_id: &str) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        super::investigations::question_stage_key(episode_id, self.focus.question.selector(), &self.focus.target)
    }
    pub fn needs_work(&self) -> bool {
        matches!(
            self.state,
            SourceReadStageState::Pending | SourceReadStageState::Deferred { .. } | SourceReadStageState::Collecting
        )
    }
    /// An admitted owner remains owed without granting a replacement admission after a cold restore.
    pub fn needs_admission(&self) -> bool {
        matches!(self.state, SourceReadStageState::Pending | SourceReadStageState::Deferred { .. })
    }
    pub fn result(&self) -> Option<&DiagnosticCaptureRef> {
        match &self.state {
            SourceReadStageState::Captured { capture } => Some(capture),
            SourceReadStageState::Answered { answer } => Some(answer.result()),
            _ => None,
        }
    }
    pub fn matches_episode(&self, episode: &DiagnosticEpisode) -> bool {
        self.is_valid()
            && self.basis_matches_episode(episode)
            && self.focus.target.telemetry_id().ok() == episode.scope.target.telemetry_id().ok()
            && (!matches!(self.state, SourceReadStageState::Recovered)
                || self.focus.question == SourceReadQuestion::TemporarySpaceContributors
                    && episode.lifecycle == DiagnosticLifecycle::Resolved)
            && match (&episode.signal, &self.focus.prerequisite, &episode.report) {
                (
                    DiagnosticSignal::CompilationAdmission { .. },
                    SourceReadPrerequisite::CompilationAdmission { report },
                    Some(DiagnosticReport::CompilationAdmission { report: original }),
                ) => report == original,
                (
                    DiagnosticSignal::OriginalObservation {
                        condition: DiagnosticOriginalCondition::MssqlQueryHistory { query: original, .. },
                        ..
                    },
                    SourceReadPrerequisite::ObservedQueryHistoryFailure { query },
                    Some(DiagnosticReport::OriginalObservation { .. }),
                ) => query.native() == *original,
                (DiagnosticSignal::LogRateGovernance { .. }, SourceReadPrerequisite::ObservedLogRateWait, None) => true,
                (
                    DiagnosticSignal::OriginalObservation {
                        condition: DiagnosticOriginalCondition::MssqlCleanupBoundary { session, transaction },
                        ..
                    },
                    SourceReadPrerequisite::ObservedCleanupBoundary {
                        session: expected_session,
                        transaction: expected_transaction,
                    },
                    Some(DiagnosticReport::OriginalObservation { .. }),
                ) => session == expected_session && transaction == expected_transaction,
                _ => false,
            }
    }
    fn basis_matches_episode(&self, episode: &DiagnosticEpisode) -> bool {
        if self.focus.question == SourceReadQuestion::TemporarySpaceContributors {
            self.focus.basis.source_scope_id == episode.first.source_scope_id
                && self.focus.basis.native_key == episode.first.native_key
        } else {
            self.focus.basis.as_ref() == &episode.first
        }
    }
    pub fn transition(&self, state: SourceReadStageState) -> Result<Self> {
        if !self.is_valid() {
            return Err(invalid());
        }
        if self.state == state {
            return Ok(self.clone());
        }
        let permitted = match self.state {
            SourceReadStageState::Pending => matches!(
                state,
                SourceReadStageState::Deferred { .. }
                    | SourceReadStageState::Collecting
                    | SourceReadStageState::Recovered
                    | SourceReadStageState::Stopped { .. }
            ),
            SourceReadStageState::Deferred { .. } => matches!(
                state,
                SourceReadStageState::Deferred { .. }
                    | SourceReadStageState::Collecting
                    | SourceReadStageState::Recovered
                    | SourceReadStageState::Stopped { .. }
            ),
            SourceReadStageState::Collecting => {
                matches!(
                    state,
                    SourceReadStageState::Captured { .. }
                        | SourceReadStageState::Answered { .. }
                        | SourceReadStageState::Stopped { .. }
                )
            }
            SourceReadStageState::Captured { .. }
            | SourceReadStageState::Answered { .. }
            | SourceReadStageState::Recovered
            | SourceReadStageState::Stopped { .. } => false,
        };
        let next = Self { focus: self.focus.clone(), state };
        if !permitted || !next.is_valid() {
            return Err(invalid());
        }
        Ok(next)
    }
}
impl DiagnosticEpisode {
    /// Construct only the logical question justified by this incident. Original-body authentication, native
    /// execution scope and source-issued cost approval are separate mandatory admission checks.
    pub fn source_read_focus(&self) -> Result<Option<SourceReadFocus>> {
        if !self.is_valid() {
            return Err(invalid());
        }
        if let Some(stage) = &self.source_read {
            return Ok(Some(stage.focus.clone()));
        }
        let (question, prerequisite) = match (&self.signal, &self.report) {
            (
                DiagnosticSignal::CompilationAdmission { .. },
                Some(DiagnosticReport::CompilationAdmission { report }),
            ) => (
                SourceReadQuestion::CompilationHistory,
                SourceReadPrerequisite::CompilationAdmission { report: report.clone() },
            ),
            (
                DiagnosticSignal::OriginalObservation {
                    condition: DiagnosticOriginalCondition::MssqlQueryHistory { query, .. },
                    ..
                },
                Some(DiagnosticReport::OriginalObservation { .. }),
            ) => (
                SourceReadQuestion::CompilationHistory,
                SourceReadPrerequisite::ObservedQueryHistoryFailure {
                    query: NativeQueryIdentity { query_id: query.id.clone(), qualifiers: query.qualifiers.clone() },
                },
            ),
            (DiagnosticSignal::LogRateGovernance { .. }, None) => {
                (SourceReadQuestion::HyperscaleLogComponents, SourceReadPrerequisite::ObservedLogRateWait)
            }
            (
                DiagnosticSignal::OriginalObservation {
                    condition: DiagnosticOriginalCondition::MssqlCleanupBoundary { session, transaction },
                    ..
                },
                Some(DiagnosticReport::OriginalObservation { .. }),
            ) => (
                SourceReadQuestion::TemporarySpaceContributors,
                SourceReadPrerequisite::ObservedCleanupBoundary {
                    session: session.clone(),
                    transaction: transaction.clone(),
                },
            ),
            _ => return Ok(None),
        };
        Ok(Some(SourceReadFocus {
            basis: Box::new(if question == SourceReadQuestion::TemporarySpaceContributors {
                self.latest.clone()
            } else {
                self.first.clone()
            }),
            target: self.scope.target.clone(),
            question,
            prerequisite,
        }))
    }

    pub fn has_owed_question(&self) -> bool {
        if self.lifecycle == DiagnosticLifecycle::Resolved
            && self.source_read.as_deref().is_some_and(|stage| {
                stage.focus.question == SourceReadQuestion::TemporarySpaceContributors && stage.needs_admission()
            })
        {
            return false;
        }
        self.source_read.as_deref().map_or_else(|| self.analysis.needs_work(), SourceReadStage::needs_work)
    }

    /// Only the separately enrolled question owes work after the original gateway answer completes.
    pub fn needs_question(&self, focus: &DiagnosticFocus) -> bool {
        self.matches_focus(focus)
            && match focus {
                DiagnosticFocus::SourceRead { .. } => {
                    self.source_read.as_deref().is_some_and(SourceReadStage::needs_work)
                }
                _ => self.source_read.is_none() && self.analysis.needs_work(),
            }
    }

    /// Reuse the exact first, caller-authenticated receipt. A later same-key request or source cannot
    /// supply the missing original member, native process, database incarnation or updateability.
    pub fn plan_source_read_focus(
        &self,
        source: &CollectionDescription,
        row: &ExportedRecord,
    ) -> Result<DiagnosticFocusPlan> {
        if !self.is_valid() {
            return Err(invalid());
        }
        if self.source_read.as_deref().is_some_and(|stage| !stage.needs_admission()) {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        let Some(focus) = self.source_read_focus()? else { return Ok(DiagnosticFocusPlan::NotRequired) };
        let context = focus.original_context(source)?;
        let Ok(execution) = SourceReadExecutionScope::from_context(focus.question, &context) else {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        self.plan_source_read_for_execution(source, row, &execution)
    }

    /// Execution authority is separately authenticated by the existing route/cost owner. It may record a newer
    /// known role without changing the original role, clocks, native member or local database/process epoch.
    pub fn plan_source_read_for_execution(
        &self,
        source: &CollectionDescription,
        row: &ExportedRecord,
        execution: &SourceReadExecutionScope,
    ) -> Result<DiagnosticFocusPlan> {
        if !self.is_valid() {
            return Err(invalid());
        }
        if self.source_read.as_deref().is_some_and(|stage| !stage.needs_admission()) {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        let Some(focus) = self.source_read_focus()? else { return Ok(DiagnosticFocusPlan::NotRequired) };
        if focus.question == SourceReadQuestion::TemporarySpaceContributors
            && self.lifecycle == DiagnosticLifecycle::Resolved
        {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        focus.basis.validate_canonical_record(source, &row.record)?;
        if !focus.matches_basis(source, row, execution)
            || self.source_read.as_deref().is_some_and(|stage| stage.focus != focus)
        {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        }
        Ok(DiagnosticFocusPlan::Ready(Box::new(DiagnosticFocus::SourceRead { focus: Box::new(focus) })))
    }

    pub fn enroll_source_read(&self, focus: SourceReadFocus) -> Result<Self> {
        if !self.is_valid() {
            return Err(invalid());
        }
        let stage = SourceReadStage { focus, state: SourceReadStageState::Pending };
        if !stage.matches_episode(self)
            || (self.source_read.is_none()
                && stage.focus.question == SourceReadQuestion::TemporarySpaceContributors
                && stage.focus.basis.as_ref() != &self.latest)
        {
            return Err(invalid());
        }
        if let Some(existing) = &self.source_read {
            if existing.focus != stage.focus {
                return Err(invalid());
            }
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.source_read = Some(Box::new(stage));
        if !next.is_valid() {
            return Err(invalid());
        }
        Ok(next)
    }

    pub fn transition_source_read(&self, state: SourceReadStageState) -> Result<Self> {
        if !self.is_valid() {
            return Err(invalid());
        }
        let mut next = self.clone();
        next.source_read = Some(Box::new(self.source_read.as_deref().ok_or_else(invalid)?.transition(state)?));
        if !next.is_valid() {
            return Err(invalid());
        }
        Ok(next)
    }

    /// The caller authenticates the completed, settled capture. Only its original typed descriptor can
    /// answer this stage; zero rows, partial coverage and unavailable sources retain their own native meaning.
    pub fn complete_source_read(
        &self,
        intent: &DiagnosticInvestigationIntent,
        source: &CollectionDescription,
        capture: &DiagnosticCaptureRef,
    ) -> Result<Self> {
        if !self.is_valid()
            || !self.needs_question(&intent.focus)
            || !intent.matches_episode(self)
            || !intent.matches_result_source(source)
            || !matches!(source.observation, SourceObservation::SourceRead { .. })
            || !source.observation.covers_request()
            || !capture.is_valid()
            || source.captured_range()?.scope_id != capture.source_scope_id
        {
            return Err(invalid());
        }
        if self
            .source_read
            .as_deref()
            .is_some_and(|stage| stage.focus.question == SourceReadQuestion::TemporarySpaceContributors)
        {
            return Err(invalid());
        }
        self.transition_source_read(SourceReadStageState::Captured { capture: capture.clone() })
    }

    /// One successful completed capture is retained independently of its potentially large row population.
    pub fn source_read_capture(&self) -> Option<&DiagnosticCaptureRef> {
        self.source_read.as_deref().and_then(SourceReadStage::result)
    }

    pub fn accepts_source_read_transition(&self, next: &Self) -> bool {
        match (&self.source_read, &next.source_read) {
            (None, None) => true,
            (None, Some(stage)) => {
                matches!(stage.state, SourceReadStageState::Pending | SourceReadStageState::Deferred { .. })
                    && self.report.as_ref().is_none_or(|report| {
                        next.report.as_ref() == Some(report)
                            && self.analysis == next.analysis
                            && self.issue == next.issue
                    })
            }
            (Some(before), Some(after)) => {
                before.focus == after.focus
                    && before.transition(after.state.clone()).is_ok()
                    && self.analysis == next.analysis
                    && match (&self.signal, &self.report, &next.report) {
                        (
                            DiagnosticSignal::OriginalObservation { .. },
                            Some(DiagnosticReport::OriginalObservation { .. }),
                            Some(DiagnosticReport::OriginalObservation { .. }),
                        ) => self.issue == next.issue,
                        _ => self.report == next.report,
                    }
            }
            (Some(_), None) => false,
        }
    }
}

impl SourceObservation {
    pub fn execution_context(&self) -> Option<&NativeReadContext> {
        match self {
            Self::SourceRead { execution, .. } | Self::PagedSourceRead { execution, .. } => Some(execution),
            _ => None,
        }
    }
}
fn cleanup_attention(row: &TelemetryRecord) -> bool {
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlCleanupHorizon(horizon),
        ..
    }) = row
    else {
        return false;
    };
    horizon.oldest_confirmed.as_deref().is_some_and(MssqlCleanupTransaction::needs_cleanup_investigation)
}
pub(super) fn same_time(left: &str, right: &str) -> bool {
    canonical_time(left).ok().zip(canonical_time(right).ok()).is_some_and(|(left, right)| left == right)
}
fn hash(value: &impl Serialize) -> Result<String> {
    serde_json::to_vec(value).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
