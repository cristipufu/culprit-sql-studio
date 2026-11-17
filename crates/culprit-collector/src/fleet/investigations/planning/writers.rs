//! A second native question starts from the sealed page and original actor frame, never a later same-ID actor.
use super::*;

impl EvidencePlanner<'_> {
    pub(super) async fn focus(
        &mut self,
        outbox: &mut Outbox,
        target: &FleetTarget,
        episode: &DiagnosticEpisode,
        requested: RecipeSelector,
    ) -> Result<Option<Box<DiagnosticFocus>>> {
        if episode.source_read.is_some() {
            return self.source_read(outbox, target, episode, requested).await;
        }
        if matches!(episode.signal, DiagnosticSignal::PageTaskWait { .. }) {
            return self.task_holders(outbox, target, episode).await;
        }
        if let Some(DiagnosticReport::PageWait { observation, .. }) = &episode.report {
            return self.page_writers(outbox, target, episode, observation).await;
        }
        if requested == RecipeSelector::PageWaitTasks {
            return self.page_tasks(outbox, target, episode).await;
        }
        let reference = &episode.latest;
        let Some((source, row)) =
            resolve_optional_evidence(outbox, &target.connection, reference, self.recovery, self.cancellation).await?
        else {
            return Ok(None);
        };
        let TelemetryRecord::Activity(original) = &row.record else {
            return Err(DeliveryProblem::Integrity.error());
        };
        Ok(ready(episode.plan_focus(&source, original, reference)?))
    }

    async fn page_writers(
        &self,
        outbox: &mut Outbox,
        target: &FleetTarget,
        episode: &DiagnosticEpisode,
        reference: &DiagnosticEvidenceRef,
    ) -> Result<Option<Box<DiagnosticFocus>>> {
        let Some((source, row)) =
            resolve_optional_evidence(outbox, &target.connection, reference, self.recovery, self.cancellation).await?
        else {
            return Ok(None);
        };
        let SourceObservation::FocusedPageWait { result, .. } = &source.observation else {
            return Err(DeliveryProblem::Integrity.error());
        };
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) =
            &row.record
        else {
            return Err(DeliveryProblem::Integrity.error());
        };
        let original = &result.focus.participant;
        // Recover the exact original Activity capture through its existing bounded cache owner before borrowing it.
        if resolve_optional_evidence(outbox, &target.connection, &original.parent, self.recovery, self.cancellation)
            .await?
            .is_none()
            || self.cancellation.is_cancelled()
        {
            return Ok(None);
        }
        let parent =
            outbox.diagnostic_input(&original.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await?;
        if parent.parent != original.parent.capture_ref() {
            return Err(DeliveryProblem::Integrity.error());
        }
        let frame = parent.activity_frame()?;
        match episode.plan_page_wait_writers(&source, page, reference, &result.focus, &frame)? {
            DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::Unsupported) => {
                // An outside-database or unsupported mapping still owes its original question on a separately
                // admitted explicit shared owner. It never authorizes a writer lookup or a new database owner.
                Ok(ready(episode.plan_focus(&parent.source, &original.original, &original.parent)?))
            }
            plan => Ok(ready(plan)),
        }
    }
}

pub(super) fn ready(plan: DiagnosticFocusPlan) -> Option<Box<DiagnosticFocus>> {
    match plan {
        DiagnosticFocusPlan::Ready(focus) => Some(focus),
        DiagnosticFocusPlan::NotRequired | DiagnosticFocusPlan::Unavailable(_) => None,
    }
}
