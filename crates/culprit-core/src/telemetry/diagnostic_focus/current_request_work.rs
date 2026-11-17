//! Current native work explains the original request without resolving its symptom or assigning its cause.
use super::*;

pub struct DiagnosticCurrentRequestWorkRow<'a> {
    pub reference: &'a DiagnosticEvidenceRef,
    pub observation: &'a OperationalObservation,
}

pub fn analyze_diagnostic_current_request_work(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticCurrentRequestWorkRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
    intent: Option<&DiagnosticInvestigationIntent>,
) -> Result<DiagnosticFocusUpdate> {
    let SourceObservation::CurrentRequestWork { snapshot, result, .. } = &source.observation else {
        return Err(invalid());
    };
    let focus = result.focus.as_deref().filter(|focus| focus.is_current_request_work_focus()).ok_or_else(invalid)?;
    if !source.observation.covers_request()
        || !result.is_valid_source(source, snapshot)
        || !input.is_valid()
        || !previous.is_valid()
        || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
        || source.target.telemetry_id()? != previous.target.telemetry_id()?
        || input.source_scope_id != source.captured_range()?.scope_id
        || rows.len() != snapshot.records as usize
        || rows.len() > MAX_FOCUSED_REQUEST_OBSERVATIONS
        || intent.is_some_and(|intent| !intent.matches_result_source(source))
    {
        return Err(invalid());
    }
    for row in rows {
        if row.reference.capture_ref() != *input
            || !row.observation.is_valid()
            || !matches!(&row.observation.fact, OperationalFact::CurrentRequestWork(point) if point.fits_source_scope(source))
        {
            return Err(invalid());
        }
        row.reference.validate_canonical_record(source, &TelemetryRecord::Operational(row.observation.clone()))?;
    }
    let mut revisions = Vec::new();
    let mut episodes = Vec::new();
    for before in &previous.episodes {
        let mut next = before.clone();
        if before.analysis.needs_work()
            && before.matches_current_request_work_focus(focus)
            && intent.is_none_or(|intent| intent.matches_episode(before))
        {
            apply_result(&mut next, result, snapshot, rows.first());
            // The original remains pinned after symptom recovery; current counters are never copied into it.
            if let Some(mut revision) = DiagnosticRevision::between(Some(before), next.clone(), analyzed_at)? {
                if !revision.inputs.contains(&focus.parent) {
                    revision.inputs.push(focus.parent.clone());
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

fn apply_result(
    episode: &mut DiagnosticEpisode,
    result: &FocusedRequestResult,
    snapshot: &StateSnapshotDescription,
    row: Option<&DiagnosticCurrentRequestWorkRow<'_>>,
) {
    if result.binding == FocusedRequestBinding::Matched
        && result.membership == FocusedRequestMembership::Observed
        && let Some(row) = row
    {
        episode.report = Some(DiagnosticReport::CurrentRequestWork { observation: row.reference.clone() });
        if snapshot.coverage_state().is_ok_and(|state| state == crate::SourceState::Collected) {
            episode.analysis = DiagnosticAnalysisStatus::Analyzed;
            episode.issue = None;
            return;
        }
    }
    // Missing, partial and unverified points cannot complete an owed explanation, including after recovery.
    episode.analysis = DiagnosticAnalysisStatus::Partial;
    episode.issue = Some(match result.binding {
        FocusedRequestBinding::ParticipantChanged | FocusedRequestBinding::ParticipantMissing => {
            DiagnosticAnalysisIssue::NativeContextGone
        }
        _ => DiagnosticAnalysisIssue::EvidenceUnavailable,
    });
}
