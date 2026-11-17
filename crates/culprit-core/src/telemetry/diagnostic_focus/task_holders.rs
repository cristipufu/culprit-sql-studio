//! Guarded current task relationships answer only their own original point question, without recovery claims.
use super::*;

pub struct DiagnosticPageTaskHolderRow<'a> {
    pub reference: &'a DiagnosticEvidenceRef,
    pub observation: &'a OperationalObservation,
}

pub fn analyze_diagnostic_page_task_holders(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticPageTaskHolderRow<'_>],
    previous: &DiagnosticCheckpoint,
    analyzed_at: &str,
    intent: Option<&DiagnosticInvestigationIntent>,
) -> Result<DiagnosticFocusUpdate> {
    let SourceObservation::PageTaskHolders { snapshot, focus, .. } = &source.observation else { return Err(invalid()) };
    let context = NativeReadContext::from_snapshot(source)?;
    if !source.observation.covers_request()
        || !input.is_valid()
        || !previous.is_valid()
        || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
        || previous.target != focus.participant.target
        || input.source_scope_id != source.captured_range()?.scope_id
        || rows.len() != snapshot.records as usize
        || rows.len() != 1
        || intent.is_some_and(|intent| !intent.matches_result_source(source))
    {
        return Err(invalid());
    }
    let row = &rows[0];
    let OperationalFact::PageTaskHolders(point) = &row.observation.fact else { return Err(invalid()) };
    if row.reference.row_ordinal != 0
        || !focus.accepts_observation(&context, point)
        || row.reference.capture_ref() != *input
    {
        return Err(invalid());
    }
    row.reference.validate_canonical_record(source, &TelemetryRecord::Operational(row.observation.clone()))?;
    let mut revisions = Vec::new();
    let mut episodes = Vec::new();
    for before in &previous.episodes {
        let mut next = before.clone();
        if before.analysis.needs_work()
            && before.matches_page_task_holder_focus(focus)
            && intent.is_none_or(|intent| intent.matches_episode(before))
        {
            apply_result(&mut next, focus, row, point);
            if let Some(revision) = DiagnosticRevision::between(Some(before), next.clone(), analyzed_at)? {
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
    focus: &PageTaskHolderFocus,
    row: &DiagnosticPageTaskHolderRow<'_>,
    point: &PageTaskHolderObservation,
) {
    episode.report = Some(DiagnosticReport::PageTaskHolders {
        observation: row.reference.clone(),
        page: Box::new(focus.page.clone()),
        parent: Box::new(focus.participant.parent.clone()),
    });
    let issue = match point.target_state {
        PageWriterTargetState::ChangedTarget => Some(DiagnosticAnalysisIssue::SourceChanged),
        PageWriterTargetState::NoLongerObserved => Some(DiagnosticAnalysisIssue::NativeContextGone),
        PageWriterTargetState::Unavailable => Some(DiagnosticAnalysisIssue::EvidenceUnavailable),
        PageWriterTargetState::CurrentMapped => point_issue(point),
    };
    episode.analysis =
        if issue.is_none() { DiagnosticAnalysisStatus::Analyzed } else { DiagnosticAnalysisStatus::Unresolved };
    episode.issue = issue;
    // The original native clock, task receipt and uncertain lifecycle never change. The finished point question
    // retires from the volatile checkpoint; this revision and all three raw receipts remain immutable history.
}

fn point_issue(point: &PageTaskHolderObservation) -> Option<DiagnosticAnalysisIssue> {
    let points = [&point.before, &point.after];
    if points.iter().any(|point| {
        point.request != PageWaitRequestState::SameStatement || point.wait != PageWaitRelation::OriginalObserved
    }) {
        return Some(DiagnosticAnalysisIssue::NativeContextGone);
    }
    if points.iter().any(|point| point.holder.relation == PageTaskHolderRelation::Unavailable) {
        return Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    }
    if points.iter().any(|point| point.holder.relation == PageTaskHolderRelation::Changed) {
        return Some(DiagnosticAnalysisIssue::SourceChanged);
    }
    points
        .iter()
        .any(|point| point.holder.relation != PageTaskHolderRelation::QualifiedRequest)
        .then_some(DiagnosticAnalysisIssue::MissingNativeIdentity)
}
