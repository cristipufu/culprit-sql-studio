//! A later compiler-admission read explains the retained affected request without asserting recovery or ownership.
use super::*;

pub struct DiagnosticCompilerRow<'a> {
    pub reference: &'a DiagnosticEvidenceRef,
    pub observation: &'a OperationalObservation,
}

pub fn analyze_diagnostic_compilation_admission(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticCompilerRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
    intent: Option<&DiagnosticInvestigationIntent>,
) -> Result<DiagnosticFocusUpdate> {
    let SourceObservation::FocusedCompilationAdmission { snapshot, result } = &source.observation else {
        return Err(invalid());
    };
    if !source.observation.covers_request()
        || !result.focus.matches_capture_scope(source)
        || !input.is_valid()
        || !previous.is_valid()
        || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
        || previous.target != result.focus.target
        || input.source_scope_id != source.captured_range()?.scope_id
        || rows.len() != snapshot.records as usize
        || !result.matches_observations(rows.iter().map(|row| row.observation))
        || intent.is_some_and(|intent| !intent.matches_result_source(source))
        || matches!(source.target, CollectionTarget::Shared(_)) && intent.is_none()
    {
        return Err(invalid());
    }
    for row in rows {
        if row.reference.capture_ref() != *input {
            return Err(invalid());
        }
        row.reference.validate_canonical_record(source, &TelemetryRecord::Operational(row.observation.clone()))?;
    }
    let mut revisions = Vec::new();
    let mut episodes = Vec::new();
    for before in &previous.episodes {
        let mut next = before.clone();
        if before.analysis.needs_work()
            && before.matches_compilation_admission_focus(&result.focus)
            && intent.is_none_or(|intent| intent.matches_episode(before))
        {
            if let Some(waiter) =
                rows.iter().find(|row| matches!(row.observation.fact, OperationalFact::CompilerWaiter(_)))
            {
                next.report = Some(DiagnosticReport::CompilationAdmission {
                    observation: waiter.reference.clone(),
                    gateways: rows
                        .iter()
                        .filter(|row| matches!(row.observation.fact, OperationalFact::CounterSet(_)))
                        .map(|row| row.reference.clone())
                        .collect(),
                });
                next.analysis = if snapshot.sources.iter().all(|source| source.complete_scope) {
                    DiagnosticAnalysisStatus::Analyzed
                } else {
                    DiagnosticAnalysisStatus::Partial
                };
                next.issue = (next.analysis == DiagnosticAnalysisStatus::Partial)
                    .then_some(DiagnosticAnalysisIssue::EvidenceUnavailable);
            } else {
                next.analysis = DiagnosticAnalysisStatus::Unresolved;
                next.issue = Some(if result.source == FocusSourceState::Changed {
                    DiagnosticAnalysisIssue::SourceChanged
                } else {
                    DiagnosticAnalysisIssue::EvidenceUnavailable
                });
            }
            // A missing current waiter or idle gateway does not prove the original request disappeared.
            if let Some(mut revision) = DiagnosticRevision::between(Some(before), next.clone(), analyzed_at)? {
                if !revision.inputs.contains(&result.focus.parent) {
                    revision.inputs.push(result.focus.parent.clone());
                }
                if let Some(DiagnosticInvestigationIntent {
                    execution: DiagnosticExecutionBinding::Shared { owner },
                    ..
                }) = intent
                    && !revision.inputs.contains(owner)
                {
                    revision.inputs.push(owner.as_ref().clone());
                }
                revision.id = revision.expected_id()?;
                if !revision.is_valid() {
                    return Err(invalid());
                }
                next = revision.episode.clone();
                revisions.push(revision);
            }
        }
        if next.retained_in_checkpoint() {
            episodes.push(next);
        }
    }
    let checkpoint = DiagnosticCheckpoint {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: previous.target.clone(),
        input: input.clone(),
        episodes,
    };
    if !checkpoint.is_valid() {
        return Err(invalid());
    }
    Ok(DiagnosticFocusUpdate { revisions, checkpoint })
}
