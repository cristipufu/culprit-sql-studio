//! A second question uses the original Activity population and one authenticated mapped-page result.
use super::*;

/// Reconstruct bounded candidates from a caller-authenticated original frame. The original page and every
/// selected request retain separate receipts; this proves neither latch ownership nor all possible writers.
pub fn plan_page_wait_writer_focus(
    page_source: &CollectionDescription,
    page: &PageWaitObservation,
    reference: &DiagnosticEvidenceRef,
    original: &ActivityWaitFocus,
    frame: &ActivityFrameEvidence<'_>,
) -> Result<DiagnosticFocusPlan> {
    verify_page(page_source, page, reference, original)?;
    if !supports_writer_target(page_source, page, original) {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::Unsupported));
    }
    if original.participant().database_writable.is_none() {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable));
    }
    if !PageWaitWriterFocus::accepts_waiter(original) {
        return Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::Unsupported));
    }
    let participant = original.participant();
    if !frame.is_qualified()
        || frame.capture_ref().as_ref().is_some_and(|capture| *capture != participant.parent.capture_ref())
        || frame.source().target != participant.target
        || frame.source().partition != participant.partition
        || frame.source().source != participant.source
        || frame.native().epoch.as_ref() != Some(&participant.epoch)
        || frame.native().database_writable != participant.database_writable
        || frame.native().at != participant.original_observed_at
    {
        return Err(invalid());
    }
    let Some(waiter) = frame.row(&participant.original.key) else {
        return if frame.read_complete() {
            Err(invalid())
        } else {
            Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable))
        };
    };
    if !waiter.reference.is_some_and(|reference| original.matches_parent(frame.source(), waiter.observation, reference))
    {
        return Err(invalid());
    }
    let candidates = original_candidates(original, frame);
    let focus = PageWaitWriterFocus {
        waiter: original.clone(),
        page: reference.clone(),
        address: page.observed_address.ok_or_else(invalid)?,
        target: page.target.clone().ok_or_else(invalid)?,
        candidates,
    };
    if !focus.matches_page(page_source, page, reference) {
        return Err(invalid());
    }
    Ok(DiagnosticFocusPlan::Ready(Box::new(DiagnosticFocus::PageWaitWriters { focus: Box::new(focus) })))
}

impl DiagnosticEpisode {
    /// Continue the same incident after its page mapping. Neither a newer Activity frame nor a different page
    /// result can replace the original population selected by that first-stage question.
    pub fn plan_page_wait_writers(
        &self,
        page_source: &CollectionDescription,
        page: &PageWaitObservation,
        reference: &DiagnosticEvidenceRef,
        original: &ActivityWaitFocus,
        frame: &ActivityFrameEvidence<'_>,
    ) -> Result<DiagnosticFocusPlan> {
        if !self.is_valid() {
            return Err(invalid());
        }
        if !self.analysis.needs_work() {
            return Ok(DiagnosticFocusPlan::NotRequired);
        }
        let Some(DiagnosticReport::PageWait { observation, .. }) = &self.report else {
            return Ok(DiagnosticFocusPlan::NotRequired);
        };
        if observation != reference || !self.matches_page_wait_focus(original) {
            return Err(invalid());
        }
        plan_page_wait_writer_focus(page_source, page, reference, original, frame)
    }
}

/// Whether this checked database page can support a writer question. This is eligibility only: callers still
/// authenticate the page and original frame, validate the exact roster and obtain native-cost admission.
pub fn supports_page_wait_writers(
    source: &CollectionDescription,
    page: &PageWaitObservation,
    original: &ActivityWaitFocus,
) -> bool {
    PageWaitWriterFocus::accepts_waiter(original) && supports_writer_target(source, page, original)
}

/// A missing writer prerequisite cannot turn another page question into a writer question. In particular,
/// outside-database and I/O evidence retain their independently admitted original/shared investigation path.
fn supports_writer_target(
    source: &CollectionDescription,
    page: &PageWaitObservation,
    original: &ActivityWaitFocus,
) -> bool {
    original.resource.kind == WaitResourceKind::MemoryPage
        && source.target == original.participant().target
        && source.partition == original.participant().partition
        && source.is_partition_qualified()
        && page.task_wait.is_none()
        && page.resolution == PageWaitResolution::ObservedAtBothBounds
        && page.observed_address.is_some()
        && page.target.as_ref().is_some_and(|target| target.writer_target_key().is_ok())
        && [&page.before_check, &page.after_check].into_iter().all(|check| {
            check.as_ref().is_some_and(|check| {
                check.request == PageWaitRequestState::SameStatement && check.wait == PageWaitRelation::OriginalObserved
            })
        })
}

fn verify_page(
    source: &CollectionDescription,
    page: &PageWaitObservation,
    reference: &DiagnosticEvidenceRef,
    original: &ActivityWaitFocus,
) -> Result<()> {
    let SourceObservation::FocusedPageWait { result, .. } = &source.observation else {
        return Err(invalid());
    };
    if !source.observation.covers_request() || result.focus != *original || !result.accepts_observation(page) {
        return Err(invalid());
    }
    verify_page_reference(source, page, reference)
}

fn original_candidates(original: &ActivityWaitFocus, frame: &ActivityFrameEvidence<'_>) -> Vec<PageWaitWriterRequest> {
    let participant = original.participant();
    let owners = participant
        .original
        .wait
        .iter()
        .flat_map(|wait| &wait.owners)
        .filter_map(ActivityWaitOwner::session)
        .collect::<std::collections::BTreeSet<_>>();
    let mut candidates = frame
        .rows()
        .filter(|row| {
            !row.observation.idle
                && row.observation.scope == ActivityScopeBasis::RequestDatabase
                && row.observation.key.request.is_some()
                && row.observation.statement_identity.is_some()
                && row.reference.is_some()
        })
        .collect::<Vec<_>>();
    // Original waiter first, exact native owner-session contexts next, then the stable native request order.
    // Selection is a bounded investigation roster, never a statement that omitted requests cannot write.
    candidates.sort_by_key(|row| {
        let priority = if row.observation.key == participant.original.key {
            0
        } else if owners.contains(&row.observation.key.session) {
            1
        } else {
            2
        };
        (priority, &row.observation.key)
    });
    candidates
        .into_iter()
        .take(MAX_PAGE_WAIT_WRITER_CANDIDATES)
        .filter_map(|row| {
            Some(PageWaitWriterRequest {
                parent: row.reference?.clone(),
                key: row.observation.key.clone(),
                statement_identity: row.observation.statement_identity.clone()?,
            })
        })
        .collect()
}
