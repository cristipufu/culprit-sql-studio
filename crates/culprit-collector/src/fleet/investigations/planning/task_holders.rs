//! The task question retains the exact paired originals. A later same-ID Activity row is not a prerequisite.
use super::*;

impl EvidencePlanner<'_> {
    pub(super) async fn task_holders(
        &self,
        outbox: &mut Outbox,
        target: &FleetTarget,
        episode: &DiagnosticEpisode,
    ) -> Result<Option<Box<DiagnosticFocus>>> {
        let DiagnosticSignal::PageTaskWait { parent, page, .. } = &episode.signal else {
            return Err(DeliveryProblem::Integrity.error());
        };
        let Some((page_source, row)) =
            resolve_optional_evidence(outbox, &target.connection, page, self.recovery, self.cancellation).await?
        else {
            return Ok(None);
        };
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(point), .. }) =
            &row.record
        else {
            return Err(DeliveryProblem::Integrity.error());
        };
        if resolve_optional_evidence(outbox, &target.connection, parent, self.recovery, self.cancellation)
            .await?
            .is_none()
            || self.cancellation.is_cancelled()
        {
            return Ok(None);
        }
        let original = outbox.diagnostic_input(&parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await?;
        if original.parent != parent.capture_ref() {
            return Err(DeliveryProblem::Integrity.error());
        }
        let plan = plan_page_task_holder_focus(&page_source, point, page, &original.activity_frame()?)?;
        let Some(focus) = writers::ready(plan) else { return Ok(None) };
        if !episode.matches_focus(&focus) {
            return Err(DeliveryProblem::Integrity.error());
        }
        Ok(Some(focus))
    }
}
