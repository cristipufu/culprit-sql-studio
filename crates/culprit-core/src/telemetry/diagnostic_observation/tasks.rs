//! Explicit admission of one authenticated native task point. Repeated task addresses do not join incidents.
use super::*;

/// The storage owner authenticates both original captures before calling this pure, receipt-checking boundary.
/// A missing/partial original cannot be repaired from a newer current frame, and absence never resolves this point.
pub fn observe_diagnostic_page_task(
    page_source: &CollectionDescription,
    page: &PageWaitObservation,
    reference: &DiagnosticEvidenceRef,
    original: &ActivityFrameEvidence<'_>,
    previous: Option<&DiagnosticCheckpoint>,
    analyzed_at: &str,
) -> Result<DiagnosticObservationUpdate> {
    validate_previous(page_source, previous, analyzed_at)?;
    let SourceObservation::PageWaitTargets { original_activity: Some(activity), .. } = &page_source.observation else {
        return Err(invalid());
    };
    if original.source().captured_range()?.scope_id != activity.source_scope_id
        || original.capture_ref().as_ref().is_some_and(|actual| actual != activity)
    {
        return Err(invalid());
    }
    let plan = plan_page_task_holder_focus(page_source, page, reference, original)?;
    let mut checkpoint = DiagnosticCheckpoint {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: page_source.target.clone(),
        input: reference.capture_ref(),
        episodes: previous.map(|previous| previous.episodes.clone()).unwrap_or_default(),
    };
    let mut revisions = Vec::new();
    let mut focuses = Vec::new();
    if let DiagnosticFocusPlan::Ready(question) = plan {
        let DiagnosticFocus::PageTaskHolders { focus } = question.as_ref() else { return Err(invalid()) };
        let mut episode = start_task_episode(original, focus)?;
        let existing = checkpoint.episodes.iter().position(|old| old.id == episode.id);
        if let Some(position) = existing {
            episode = checkpoint.episodes[position].clone();
            if !episode.matches_focus(&question) {
                return Err(invalid());
            }
        } else {
            if checkpoint.episodes.len() >= MAX_DIAGNOSTIC_EPISODES {
                return Err(CollectionStop::RowLimit.error());
            }
            record_transition(&mut revisions, None, &mut episode, analyzed_at)?;
            checkpoint.episodes.push(episode.clone());
        }
        if episode.analysis.needs_work() {
            focuses.push((episode.id, *question));
        }
    }
    if !checkpoint.is_valid() {
        return Err(invalid());
    }
    Ok(DiagnosticObservationUpdate { revisions, checkpoint, focuses })
}

fn validate_previous(
    source: &CollectionDescription,
    previous: Option<&DiagnosticCheckpoint>,
    analyzed_at: &str,
) -> Result<()> {
    if !valid_utc_instant(analyzed_at)
        || previous.is_some_and(|previous| {
            !previous.is_valid()
                || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
                || previous.target.telemetry_id().ok() != source.target.telemetry_id().ok()
        })
    {
        return Err(invalid());
    }
    Ok(())
}

fn start_task_episode(frame: &ActivityFrameEvidence<'_>, focus: &PageTaskHolderFocus) -> Result<DiagnosticEpisode> {
    let signal = DiagnosticSignal::PageTaskWait {
        key: focus.participant.original.key.clone(),
        statement_identity: focus.participant.statement_identity.clone(),
        parent: Box::new(focus.participant.parent.clone()),
        page: Box::new(focus.page.clone()),
        task: Box::new(focus.task.clone()),
        database_writable: focus.participant.database_writable.ok_or_else(invalid)?,
    };
    let episode = DiagnosticEpisode::start(
        DiagnosticScope::from_source(frame.source())?,
        signal,
        focus.task.observed_at.clone(),
        focus.page.clone(),
    )?;
    if !episode.matches_page_task_holder_focus(focus) {
        return Err(invalid());
    }
    Ok(episode)
}
