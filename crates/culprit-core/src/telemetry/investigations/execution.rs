//! A shared owner is independently authenticated; it never replaces the database participant's original scope.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticExecutionBinding {
    Database,
    NativeRead { execution: Box<NativeSourceReadScope> },
    Shared { owner: Box<DiagnosticEvidenceRef> },
}

/// A caller-authenticated sealed row. The pure constructor checks consistency, not storage authenticity.
pub struct DiagnosticSharedOwner<'a> {
    pub source: &'a CollectionDescription,
    pub row: &'a ExportedRecord,
    pub reference: &'a DiagnosticEvidenceRef,
}

impl DiagnosticExecutionBinding {
    pub(super) fn matches_focus(&self, scope: &RecipeCostScope, focus: &DiagnosticFocus) -> bool {
        match (self, scope) {
            (Self::Database, RecipeCostScope::Activity(scope)) => {
                let Some(participant) = focus.activity_participant() else { return false };
                scope.source == participant.source
                    && scope.native_epoch.as_deref() == Some(participant.epoch.as_str())
                    && scope.partition == participant.partition
                    && scope.target.telemetry_id().ok() == participant.target.telemetry_id().ok()
            }
            (Self::Shared { owner }, RecipeCostScope::SharedResource { target, .. }) => {
                let Some(participant) = focus.activity_participant() else { return false };
                owner.is_valid()
                    && owner.stream == TelemetryStream::OperationalSamples
                    && matches!(owner.native_key, NativeRecordIdentity::Operational { .. })
                    && matches!(focus, DiagnosticFocus::PageWait { .. } | DiagnosticFocus::CompilationAdmission { .. })
                    && participant.is_valid_for(&CollectionSubject::Shared { resource: target.clone() })
            }
            (Self::NativeRead { execution }, RecipeCostScope::NativeRead(scope)) => {
                execution.is_valid()
                    && execution.key().ok() == scope.key().ok()
                    && matches!(focus, DiagnosticFocus::SourceRead { focus } if execution.matches_target(&focus.target))
            }
            _ => false,
        }
    }
}

impl DiagnosticInvestigationIntent {
    pub fn subject(&self) -> CollectionSubject {
        self.qualification.scope.subject()
    }

    pub fn selection(&self) -> Result<CollectionSelection> {
        Ok(match (&self.focus, &self.execution) {
            (DiagnosticFocus::PageWait { focus }, DiagnosticExecutionBinding::Shared { .. }) => {
                CollectionSelection::SharedPageWaitFocus {
                    focus: focus.clone(),
                    partition: self.qualification.scope.partition().clone(),
                }
            }
            (DiagnosticFocus::CompilationAdmission { focus }, DiagnosticExecutionBinding::Shared { .. }) => {
                CollectionSelection::SharedCompilationAdmissionFocus {
                    focus: focus.clone(),
                    partition: self.qualification.scope.partition().clone(),
                }
            }
            (DiagnosticFocus::SourceRead { focus }, DiagnosticExecutionBinding::NativeRead { execution }) => {
                CollectionSelection::SourceRead { focus: focus.clone(), execution: execution.clone() }
            }
            _ => self.focus.activity_selection().ok_or_else(invalid)?,
        })
    }

    /// A single consistency path for a caller-authenticated original. Non-request source questions still use
    /// a genuine incident receipt, without fabricating an ActivityRequestFocus or borrowing newer metadata.
    pub fn matches_parent_record(&self, source: &CollectionDescription, row: &ExportedRecord) -> bool {
        if !self.is_valid() {
            return false;
        }
        match (&self.focus, &self.execution) {
            (DiagnosticFocus::SourceRead { focus }, DiagnosticExecutionBinding::NativeRead { execution }) => {
                focus.matches_basis(source, row, execution)
            }
            _ => matches!(&row.record, TelemetryRecord::Activity(original)
                if self.focus.matches_parent(source, original, self.focus.parent())),
        }
    }

    /// Recheck the exact owner artifact after the caller resolves its manifest, membership and row ordinal.
    pub fn matches_shared_owner(&self, source: &CollectionDescription, row: &ExportedRecord) -> bool {
        let DiagnosticExecutionBinding::Shared { owner } = &self.execution else {
            return false;
        };
        if !self.is_valid() || !self.qualification.scope.matches_source(source) {
            return false;
        }
        let TelemetryRecord::Operational(observation) = &row.record else {
            return false;
        };
        let Some(snapshot) = source.observation.snapshot() else {
            return false;
        };
        snapshot.records > 0
            && snapshot.sources.iter().any(|coverage| {
                coverage.source == observation.source
                    && coverage.records > 0
                    && matches!(coverage.state, crate::SourceState::Collected | crate::SourceState::Partial)
            })
            && DiagnosticEvidenceRef::from_export(source, &owner.manifest_sha256, row, owner.row_ordinal)
                .is_ok_and(|reference| reference == **owner)
    }

    /// A newer original episode may still owe this question; the execution resource does not become its scope.
    pub fn matches_episode(&self, episode: &DiagnosticEpisode) -> bool {
        self.is_valid()
            && episode.is_valid()
            && episode.id == self.episode_id
            && super::super::diagnostics::positive(&episode.revision)
                >= super::super::diagnostics::positive(&self.episode_revision)
            && episode.matches_focus(&self.focus)
            && match &self.qualification.scope {
                RecipeCostScope::Activity(scope) => scope.key().ok() == episode.scope.key().ok(),
                RecipeCostScope::SharedResource { .. } | RecipeCostScope::NativeRead(_) => true,
            }
    }

    /// A paired population may add independently authenticated task-point episodes from the same exact original
    /// frame. This is structural permission only; its actual raw page receipt and original row must still join.
    pub fn permits_result_episode(&self, episode: &DiagnosticEpisode) -> bool {
        if self.matches_episode(episode) {
            return true;
        }
        let DiagnosticFocus::PageWaitTasks { focus, .. } = &self.focus else { return false };
        let RecipeCostScope::Activity(scope) = &self.qualification.scope else { return false };
        self.is_valid()
            && episode.is_valid()
            && episode.scope == *scope
            && matches!(&episode.signal, DiagnosticSignal::PageTaskWait { parent, .. }
                if parent.capture_ref() == focus.parent.capture_ref())
    }

    /// Native result facts must retain this exact question and the separately admitted execution generation.
    pub fn matches_result_source(&self, source: &CollectionDescription) -> bool {
        let Ok(selection) = self.selection() else { return false };
        self.is_valid()
            && match &self.execution {
                DiagnosticExecutionBinding::NativeRead { execution } => {
                    execution.matches_target(&source.target)
                        && source.partition == SourcePartition::Target
                        && source
                            .observation
                            .execution_context()
                            .is_some_and(|context| execution.matches_context(context))
                }
                _ => {
                    source.target == self.qualification.scope.target()
                        && source.partition == *self.qualification.scope.partition()
                }
            }
            && source.matches_request(&CollectionRequest {
                subject: self.subject(),
                selection,
                budget: self.budget.clone(),
            })
    }
}
