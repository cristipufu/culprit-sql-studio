//! Finite source questions keep logical data identity separate from the qualified execution/cost owner.
use super::*;

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
}
impl SourceReadQuestion {
    pub const fn dataset(self) -> SnapshotDataset {
        match self {
            Self::CompilationHistory => SnapshotDataset::QueryCompilation,
        }
    }
    pub const fn selector(self) -> RecipeSelector {
        RecipeSelector::Collection(TelemetryCollection::Snapshot(self.dataset()))
    }
    pub fn accepts_dataset(self, dataset: SnapshotDataset) -> bool {
        self.dataset() == dataset
    }
}

/// An exact genuine incident receipt justifies a finite historical context question. No Activity/request/SQL
/// envelope is invented for source-global compiler history. The caller authenticates this original independently.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SourceReadFocus {
    pub basis: Box<DiagnosticEvidenceRef>,
    /// Logical dataset namespace. An admitted native execution partition never replaces it.
    pub target: CollectionTarget,
    pub question: SourceReadQuestion,
    pub prerequisite: Box<CompilationAdmissionReport>,
}
impl SourceReadFocus {
    pub fn is_valid(&self) -> bool {
        self.basis.is_valid()
            && self.prerequisite.is_valid()
            && self.basis.stream == TelemetryStream::ActivityObservations
            && matches!(self.basis.native_key, NativeRecordIdentity::Activity(_))
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
        let (
            Ok(original),
            SourceObservation::FocusedCompilationAdmission { result, .. },
            TelemetryRecord::Activity(actor),
        ) = (NativeReadContext::from_activity(original_source), &source.observation, &original_row.record)
        else {
            return false;
        };
        let Ok(execution) = NativeSourceReadScope::from_context(&original) else { return false };
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
            && self.prerequisite.matches_input(source, rows)
    }
    pub fn matches_basis(
        &self,
        source: &CollectionDescription,
        row: &ExportedRecord,
        execution: &NativeSourceReadScope,
    ) -> bool {
        self.is_valid()
            && execution.matches_target(&self.target)
            && source.target.telemetry_id().ok() == self.target.telemetry_id().ok()
            && matches!(&row.record, TelemetryRecord::Activity(original)
                if original.scope == ActivityScopeBasis::RequestDatabase
                    && original.wait.as_ref().is_some_and(|wait| wait.class == Some(ActivityWaitClass::CompilationAdmission)))
            && NativeReadContext::from_activity(source).is_ok_and(|context| execution.matches_context(&context))
            && self.basis.validate_canonical_record(source, &row.record).is_ok()
            && self.basis.capture == row.capture
            && row.batch_id == self.basis.batch_id
            && row.payload_sha256().is_ok_and(|digest| digest == self.basis.payload_sha256)
    }
}

/// The completed waiter/gateway answer stays immutable. This separately owed question transitions only through
/// revision/event history, and retains one successful capture rather than a per-row compiler roster.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SourceReadStage {
    pub focus: SourceReadFocus,
    pub state: SourceReadStageState,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum SourceReadStageState {
    Pending,
    Deferred { issue: DiagnosticAnalysisIssue },
    Collecting,
    Captured { capture: DiagnosticCaptureRef },
    Stopped { issue: DiagnosticAnalysisIssue },
}
impl SourceReadStage {
    pub fn is_valid(&self) -> bool {
        self.focus.is_valid()
            && match &self.state {
                SourceReadStageState::Captured { capture } => {
                    capture.is_valid()
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
            _ => None,
        }
    }
    pub fn matches_episode(&self, episode: &DiagnosticEpisode) -> bool {
        self.is_valid()
            && self.focus.basis.as_ref() == &episode.first
            && self.focus.target.telemetry_id().ok() == episode.scope.target.telemetry_id().ok()
            && matches!(episode.signal, DiagnosticSignal::CompilationAdmission { .. })
            && matches!(&episode.report, Some(DiagnosticReport::CompilationAdmission { report }) if report == &self.focus.prerequisite)
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
                    | SourceReadStageState::Stopped { .. }
            ),
            SourceReadStageState::Deferred { .. } => matches!(
                state,
                SourceReadStageState::Deferred { .. }
                    | SourceReadStageState::Collecting
                    | SourceReadStageState::Stopped { .. }
            ),
            SourceReadStageState::Collecting => {
                matches!(state, SourceReadStageState::Captured { .. } | SourceReadStageState::Stopped { .. })
            }
            SourceReadStageState::Captured { .. } | SourceReadStageState::Stopped { .. } => false,
        };
        let next = Self { focus: self.focus.clone(), state };
        if !permitted || !next.is_valid() {
            return Err(invalid());
        }
        Ok(next)
    }
}
impl DiagnosticEpisode {
    pub fn has_owed_question(&self) -> bool {
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

    /// Reuse the exact first, caller-authenticated Activity receipt. A later same-key request or source cannot
    /// supply the missing original member, native process, database incarnation or updateability.
    pub fn plan_source_read_focus(
        &self,
        source: &CollectionDescription,
        row: &ExportedRecord,
    ) -> Result<DiagnosticFocusPlan> {
        if !self.is_valid() {
            return Err(invalid());
        }
        if !matches!(self.signal, DiagnosticSignal::CompilationAdmission { .. })
            || !matches!(self.report, Some(DiagnosticReport::CompilationAdmission { .. }))
            || self.source_read.as_deref().is_some_and(|stage| !stage.needs_admission())
        {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        self.first.validate_canonical_record(source, &row.record)?;
        let Some(DiagnosticReport::CompilationAdmission { report }) = &self.report else { return Err(invalid()) };
        let focus = SourceReadFocus {
            basis: Box::new(self.first.clone()),
            target: self.scope.target.clone(),
            question: SourceReadQuestion::CompilationHistory,
            prerequisite: report.clone(),
        };
        let context = NativeReadContext::from_activity(source)?;
        let Ok(execution) = NativeSourceReadScope::from_context(&context) else {
            return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity));
        };
        if !focus.matches_basis(source, row, &execution)
            || self.source_read.as_deref().is_some_and(|stage| stage.focus != focus)
        {
            return Err(invalid());
        }
        Ok(DiagnosticFocusPlan::Ready(Box::new(DiagnosticFocus::SourceRead { focus: Box::new(focus) })))
    }

    pub fn enroll_source_read(&self, focus: SourceReadFocus) -> Result<Self> {
        if !self.is_valid() {
            return Err(invalid());
        }
        let stage = SourceReadStage { focus, state: SourceReadStageState::Pending };
        if !stage.matches_episode(self) {
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
                    && self.report == next.report
                    && self.analysis == next.analysis
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
