//! A shared owner is independently authenticated; it never replaces the database participant's original scope.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticExecutionBinding {
    Database,
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
        let participant = focus.participant();
        match (self, scope) {
            (Self::Database, RecipeCostScope::Activity(scope)) => {
                scope.source == participant.source
                    && scope.native_epoch.as_deref() == Some(participant.epoch.as_str())
                    && scope.partition == participant.partition
                    && scope.target.telemetry_id().ok() == participant.target.telemetry_id().ok()
            }
            (Self::Shared { owner }, RecipeCostScope::SharedResource { target, .. }) => {
                owner.is_valid()
                    && owner.stream == TelemetryStream::OperationalSamples
                    && matches!(owner.native_key, NativeRecordIdentity::Operational { .. })
                    && matches!(focus, DiagnosticFocus::PageWait { .. } | DiagnosticFocus::CompilationAdmission { .. })
                    && participant.is_valid_for(&CollectionSubject::Shared { resource: target.clone() })
            }
            _ => false,
        }
    }
}

impl DiagnosticInvestigationIntent {
    pub fn subject(&self) -> CollectionSubject {
        self.qualification.scope.subject()
    }

    pub fn selection(&self) -> CollectionSelection {
        match (&self.focus, &self.execution) {
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
            _ => self.focus.database_selection(),
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
                RecipeCostScope::SharedResource { .. } => true,
            }
    }

    /// Native result facts must retain this exact question and the separately admitted execution generation.
    pub fn matches_result_source(&self, source: &CollectionDescription) -> bool {
        self.is_valid()
            && source.target == self.qualification.scope.target()
            && source.partition == *self.qualification.scope.partition()
            && source.matches_request(&CollectionRequest {
                subject: self.subject(),
                selection: self.selection(),
                budget: self.budget.clone(),
            })
    }
}
