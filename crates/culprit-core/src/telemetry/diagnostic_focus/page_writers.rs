//! Writer-plan evidence continues the original wait incident; it never establishes a latch owner or recovery.
use super::*;

pub struct DiagnosticPageWriterRow<'a> {
    pub reference: &'a DiagnosticEvidenceRef,
    pub observation: &'a OperationalObservation,
}

/// The caller authenticates the admitted intent and its exact original Activity/page prerequisites. This pure
/// transition checks native result consistency and receipt derivation without replacing those sealed originals.
pub fn analyze_diagnostic_page_writers(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticPageWriterRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
    intent: Option<&DiagnosticInvestigationIntent>,
) -> Result<DiagnosticFocusUpdate> {
    let SourceObservation::PageWaitWriters { snapshot, focus, .. } = &source.observation else {
        return Err(invalid());
    };
    let context = NativeReadContext::from_snapshot(source)?;
    if !source.observation.covers_request()
        || !input.is_valid()
        || !previous.is_valid()
        || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
        || previous.target != focus.waiter.participant().target
        || input.source_scope_id != source.captured_range()?.scope_id
        || rows.len() != snapshot.records as usize
        || rows.len() > 1
        || rows.iter().any(|row| {
            !matches!(&row.observation.fact, OperationalFact::PageWaitWriters(point) if focus.accepts_observation(&context, point))
        })
        || intent.is_some_and(|intent| !intent.matches_result_source(source))
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
            && before.matches_page_wait_writer_focus(focus)
            && intent.is_none_or(|intent| intent.matches_episode(before))
        {
            apply_writer_result(&mut next, source, focus, rows.first());
            if let Some(mut revision) = DiagnosticRevision::between(Some(before), next.clone(), analyzed_at)? {
                for reference in (DiagnosticFocus::PageWaitWriters { focus: focus.clone() }).references() {
                    if !revision.inputs.contains(reference) {
                        revision.inputs.push(reference.clone());
                    }
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

fn apply_writer_result(
    episode: &mut DiagnosticEpisode,
    source: &CollectionDescription,
    focus: &PageWaitWriterFocus,
    row: Option<&DiagnosticPageWriterRow<'_>>,
) {
    let Some(row) = row else {
        episode.analysis = DiagnosticAnalysisStatus::Unresolved;
        episode.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
        return;
    };
    let OperationalFact::PageWaitWriters(point) = &row.observation.fact else { return };
    let blocking_contexts = match &episode.report {
        Some(DiagnosticReport::PageWait { blocking_contexts, .. })
        | Some(DiagnosticReport::PageWaitWriters { blocking_contexts, .. }) => blocking_contexts.clone(),
        _ => vec![],
    };
    episode.report = Some(DiagnosticReport::PageWaitWriters {
        observation: row.reference.clone(),
        page: Box::new(focus.page.clone()),
        candidates: focus.candidates.iter().map(|candidate| candidate.parent.clone()).collect(),
        blocking_contexts,
    });
    let incomplete = source
        .observation
        .snapshot()
        .is_none_or(|snapshot| snapshot.sources.iter().any(|source| !source.complete_scope))
        || point.candidates.iter().any(|candidate| candidate.relation == PageWriterRelation::Unavailable);
    let issue = match point.target_state {
        PageWriterTargetState::ChangedTarget => Some(DiagnosticAnalysisIssue::SourceChanged),
        PageWriterTargetState::NoLongerObserved | PageWriterTargetState::Unavailable => {
            Some(DiagnosticAnalysisIssue::EvidenceUnavailable)
        }
        PageWriterTargetState::CurrentMapped if incomplete => Some(DiagnosticAnalysisIssue::EvidenceUnavailable),
        PageWriterTargetState::CurrentMapped
            if point.candidates.iter().any(|candidate| candidate.relation == PageWriterRelation::Unsupported) =>
        {
            Some(DiagnosticAnalysisIssue::Unsupported)
        }
        PageWriterTargetState::CurrentMapped => None,
    };
    episode.analysis = match (point.target_state, &issue) {
        (PageWriterTargetState::CurrentMapped, None) => DiagnosticAnalysisStatus::Analyzed,
        (PageWriterTargetState::CurrentMapped, Some(DiagnosticAnalysisIssue::EvidenceUnavailable)) => {
            DiagnosticAnalysisStatus::Partial
        }
        _ => DiagnosticAnalysisStatus::Unresolved,
    };
    episode.issue = issue;
    // Native lifecycle, last-seen clocks, peak points and any earlier recovery proof remain unchanged. A missing
    // or different current plan/page says nothing about whether the original waiting request recovered.
}
