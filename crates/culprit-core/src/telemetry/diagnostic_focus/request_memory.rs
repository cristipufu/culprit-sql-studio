//! Generation-bound grant follow-ups retain a later native point without rewriting the original symptom.
use super::*;

pub struct DiagnosticMemoryRow<'a> {
    pub reference: &'a DiagnosticEvidenceRef,
    pub observation: &'a ExecutionGrantObservation,
}

pub fn analyze_diagnostic_memory(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticMemoryRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
    intent: Option<&DiagnosticInvestigationIntent>,
) -> Result<DiagnosticFocusUpdate> {
    let SourceObservation::FocusedRequestMemory { snapshot, result } = &source.observation else {
        return Err(invalid());
    };
    if !source.observation.covers_request()
        || !input.is_valid()
        || !previous.is_valid()
        || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
        || source.target.telemetry_id()? != previous.target.telemetry_id()?
        || input.source_scope_id != source.captured_range()?.scope_id
        || rows.len() > 1
        || rows.len() != snapshot.records as usize
        || intent.is_some_and(|intent| !intent.matches_result_source(source))
    {
        return Err(invalid());
    }
    for row in rows {
        if row.reference.capture_ref() != *input || !result.accepts_observation(row.observation) {
            return Err(invalid());
        }
        let exported = ExportedRecord::new(
            source,
            &input.capture,
            TelemetryRecord::Operational(OperationalObservation {
                source: row.observation.source.clone(),
                key: row.observation.key()?,
                fact: OperationalFact::ExecutionGrant(Box::new(row.observation.clone())),
            }),
        )?;
        if DiagnosticEvidenceRef::from_export(source, &input.manifest_sha256, &exported, row.reference.row_ordinal)?
            != *row.reference
        {
            return Err(invalid());
        }
    }
    let mut revisions = Vec::new();
    let mut episodes = Vec::new();
    for before in &previous.episodes {
        let mut next = before.clone();
        if before.analysis.needs_work()
            && before.matches_request_memory_focus(&result.focus)
            && intent.is_none_or(|intent| intent.matches_episode(before))
        {
            if let Some(row) = rows.first() {
                next.report = Some(DiagnosticReport::RequestMemory { observation: row.reference.clone() });
                next.analysis = DiagnosticAnalysisStatus::Analyzed;
                next.issue = None;
            } else {
                next.analysis = DiagnosticAnalysisStatus::Unresolved;
                next.issue = Some(if result.source == FocusSourceState::Changed {
                    DiagnosticAnalysisIssue::SourceChanged
                } else {
                    DiagnosticAnalysisIssue::EvidenceUnavailable
                });
            }
            // No grant row establishes neither request disappearance nor recovery from the original wait.
            if let Some(mut revision) = DiagnosticRevision::between(Some(before), next.clone(), analyzed_at)? {
                if !revision.inputs.contains(&result.focus.parent) {
                    revision.inputs.push(result.focus.parent.clone());
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
