//! A current lock closure may explain its exact original request; it never proves historical recovery.
use super::*;
use std::collections::BTreeSet;

pub fn analyze_diagnostic_blocking(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticActivityRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
    intent: Option<&DiagnosticInvestigationIntent>,
) -> Result<DiagnosticFocusUpdate> {
    let SourceObservation::FocusedBlocking { result, .. } = &source.observation else { return Err(invalid()) };
    if !input.is_valid()
        || !previous.is_valid()
        || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
        || source.target.telemetry_id()? != previous.target.telemetry_id()?
        || input.source_scope_id != source.captured_range()?.scope_id
        || intent.is_some_and(|intent| !intent.matches_result_source(source))
    {
        return Err(invalid());
    }
    let borrowed = rows
        .iter()
        .map(|row| {
            row.verify(source, input)?;
            Ok(ActivityFrameRow { observation: row.observation, reference: Some(row.reference) })
        })
        .collect::<Result<Vec<_>>>()?;
    let frame = ActivityFrameEvidence::new(source, &borrowed, true)?;
    if !result.accepts_frame(&frame) {
        return Err(invalid());
    }
    let mut revisions = Vec::new();
    let mut episodes = Vec::new();
    for before in &previous.episodes {
        let mut next = before.clone();
        if before.analysis.needs_work()
            && before.matches_blocking_focus(&result.focus)
            && intent.is_none_or(|intent| intent.matches_episode(before))
        {
            if result.binding == FocusedRequestBinding::Matched
                && result.membership == FocusedRequestMembership::Observed
                && let Some(waiter) = frame.row(&result.focus.original.key).and_then(|row| row.reference)
            {
                next.report = Some(DiagnosticReport::Blocking {
                    observation: waiter.clone(),
                    participants: rows
                        .iter()
                        .filter(|row| row.reference != waiter)
                        .map(|row| row.reference.clone())
                        .collect(),
                });
                let explained = result.has_complete_owner_context(&frame);
                next.analysis =
                    if explained { DiagnosticAnalysisStatus::Analyzed } else { DiagnosticAnalysisStatus::Partial };
                next.issue = (!explained).then_some(if result.closure.is_limited() {
                    DiagnosticAnalysisIssue::BudgetExhausted
                } else {
                    DiagnosticAnalysisIssue::EvidenceUnavailable
                });
            } else {
                next.analysis = DiagnosticAnalysisStatus::Partial;
                next.issue = Some(match result.binding {
                    FocusedRequestBinding::ParticipantMissing | FocusedRequestBinding::ParticipantChanged => {
                        DiagnosticAnalysisIssue::NativeContextGone
                    }
                    _ => DiagnosticAnalysisIssue::EvidenceUnavailable,
                });
            }
            // All rows describe this later bounded read. Disappearance/reuse at its final guard cannot resolve
            // the original incident, and an unqualified original is never upgraded by current native authority.
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

impl DiagnosticReport {
    /// The caller authenticates storage once, then the same source-owned closure predicate checks every row.
    pub fn matches_blocking_input<'a>(
        &self,
        episode: &DiagnosticEpisode,
        source: &CollectionDescription,
        rows: impl IntoIterator<Item = (&'a DiagnosticEvidenceRef, &'a ActivityObservation)>,
    ) -> bool {
        let Self::Blocking { observation, .. } = self else { return false };
        let SourceObservation::FocusedBlocking { result, .. } = &source.observation else { return false };
        let rows = rows
            .into_iter()
            .map(|(reference, observation)| ActivityFrameRow { observation, reference: Some(reference) })
            .collect::<Vec<_>>();
        let Ok(frame) = ActivityFrameEvidence::new(source, &rows, true) else { return false };
        result.binding == FocusedRequestBinding::Matched
            && result.accepts_frame(&frame)
            && episode.matches_blocking_focus(&result.focus)
            && (episode.analysis != DiagnosticAnalysisStatus::Analyzed || result.has_complete_owner_context(&frame))
            && frame.row(&result.focus.original.key).and_then(|row| row.reference) == Some(observation)
            && self.references().map(DiagnosticEvidenceRef::key).collect::<Result<BTreeSet<_>>>().ok()
                == rows
                    .iter()
                    .filter_map(|row| row.reference)
                    .map(DiagnosticEvidenceRef::key)
                    .collect::<Result<BTreeSet<_>>>()
                    .ok()
    }
}
