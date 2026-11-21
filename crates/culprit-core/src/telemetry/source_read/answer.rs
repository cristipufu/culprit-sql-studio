//! A closed answer preserves one source family's interpretation without a generic payload or second lifecycle.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "answer", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum SourceReadAnswer {
    TemporaryResources(Box<TemporaryResourceAnswer>),
    QueryCompilation(Box<QueryCompilationAnswer>),
}
impl SourceReadAnswer {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::TemporaryResources(answer) => answer.is_valid(),
            Self::QueryCompilation(answer) => answer.is_valid(),
        }
    }
    pub fn question(&self) -> SourceReadQuestion {
        match self {
            Self::TemporaryResources(_) => SourceReadQuestion::TemporarySpaceContributors,
            Self::QueryCompilation(_) => SourceReadQuestion::CompilationHistory,
        }
    }
    pub fn basis(&self) -> &DiagnosticEvidenceRef {
        match self {
            Self::TemporaryResources(answer) => &answer.basis,
            Self::QueryCompilation(answer) => &answer.basis,
        }
    }
    pub fn result(&self) -> &DiagnosticCaptureRef {
        match self {
            Self::TemporaryResources(answer) => &answer.result,
            Self::QueryCompilation(answer) => &answer.result,
        }
    }
    pub fn references(&self) -> std::vec::IntoIter<&DiagnosticEvidenceRef> {
        let references: Vec<_> = match self {
            Self::TemporaryResources(answer) => answer.references().collect(),
            Self::QueryCompilation(answer) => answer.references().collect(),
        };
        references.into_iter()
    }
}

impl DiagnosticEpisode {
    /// One closed answer dispatcher serves offline analysis and the canonical journal recheck.
    pub fn complete_source_read_from_originals(
        &self,
        intent: &DiagnosticInvestigationIntent,
        original_source: &CollectionDescription,
        original: &ExportedRecord,
        source: &CollectionDescription,
        capture: &DiagnosticCaptureRef,
        rows: &[(&DiagnosticEvidenceRef, &ExportedRecord)],
    ) -> Result<Self> {
        let DiagnosticFocus::SourceRead { focus } = &intent.focus else { return Err(invalid()) };
        if !intent.matches_parent_record(original_source, original) {
            return Err(invalid());
        }
        match &focus.prerequisite {
            SourceReadPrerequisite::ObservedCleanupBoundary { .. } => {
                self.complete_temporary_resource_read(intent, original_source, original, source, capture, rows)
            }
            SourceReadPrerequisite::ObservedQueryHistoryFailure { .. } => {
                self.complete_query_compilation_read(intent, original_source, original, source, capture, rows)
            }
            _ if rows.is_empty() => self.complete_source_read(intent, source, capture),
            _ => Err(invalid()),
        }
    }
}
