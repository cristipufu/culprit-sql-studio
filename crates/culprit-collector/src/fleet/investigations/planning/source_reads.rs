//! Exact original capture authority is recovered once, separately from the current execution route and cost proof.
use super::*;

impl EvidencePlanner<'_> {
    pub(super) async fn source_read(
        &mut self,
        outbox: &mut Outbox,
        target: &FleetTarget,
        episode: &DiagnosticEpisode,
        requested: RecipeSelector,
    ) -> Result<Option<Box<DiagnosticFocus>>> {
        let Some(stage) = &episode.source_read else { return Err(DeliveryProblem::Integrity.error()) };
        if !stage.needs_admission() || requested != stage.focus.question.selector() {
            return Ok(None);
        }
        let Some((source, row)) =
            resolve_optional_evidence(outbox, &target.connection, &stage.focus.basis, self.recovery, self.cancellation)
                .await?
        else {
            return Ok(None);
        };
        match episode.plan_source_read_focus(&source, &row)? {
            DiagnosticFocusPlan::Ready(focus) => {
                let DiagnosticFocus::SourceRead { focus: source_focus } = focus.as_ref() else {
                    return Err(DeliveryProblem::Integrity.error());
                };
                if self.execution_owners.len() >= MAX_QUESTIONS_PER_PASS {
                    return Err(DeliveryProblem::Capacity.error());
                }
                let execution = NativeSourceReadScope::from_context(&NativeReadContext::from_activity(&source)?)?;
                self.execution_owners.insert(source_focus.key()?, execution);
                Ok(Some(focus))
            }
            DiagnosticFocusPlan::Unavailable(issue) => {
                crate::diagnostics::set_source_read_issue(outbox, &stage.focus, &episode.id, issue).await?;
                Ok(None)
            }
            DiagnosticFocusPlan::NotRequired => Ok(None),
        }
    }
}
