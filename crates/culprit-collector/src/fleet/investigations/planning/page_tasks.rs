//! A population question uses its first authenticated frame once per bounded planning pass.
use super::*;

impl EvidencePlanner<'_> {
    pub(super) async fn page_tasks(
        &mut self,
        outbox: &mut Outbox,
        target: &FleetTarget,
        episode: &DiagnosticEpisode,
    ) -> Result<Option<Box<DiagnosticFocus>>> {
        let reference = &episode.first;
        let capture = reference.capture_ref();
        let key = capture.key()?;
        if !self.population_frames.contains_key(&key) {
            if self.population_frames.len() == MAX_QUESTIONS_PER_PASS {
                return Err(DeliveryProblem::Capacity.error());
            }
            let Some((source, _)) =
                resolve_optional_evidence(outbox, &target.connection, reference, self.recovery, self.cancellation)
                    .await?
            else {
                return Ok(None);
            };
            let input = outbox.diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await?;
            if input.parent != capture || input.source != source {
                return Err(DeliveryProblem::Integrity.error());
            }
            input.activity_frame()?;
            self.population_frames.insert(key.clone(), input);
        }
        let input = self.population_frames.get(&key).ok_or_else(|| DeliveryProblem::Integrity.error())?;
        let frame = input.activity_frame()?;
        Ok(writers::ready(episode.plan_page_wait_tasks_focus(&frame)?))
    }
}
