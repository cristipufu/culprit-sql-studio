//! A retired I/O future cannot overwrite a journal transaction that completed on its owned worker.
use super::{
    driver::DeliveryPolicy,
    journal::{DeliveryJournal, PendingRange},
};
use culprit_core::{
    Result,
    telemetry::{DeliveryEntry, DeliveryPhase, DeliveryProblem},
};
pub(super) async fn finish(
    journal: &DeliveryJournal,
    id: &str,
    mut entry: PendingRange,
    operation: std::result::Result<Result<()>, tokio::time::error::Elapsed>,
    now: i64,
    policy: DeliveryPolicy,
) -> Result<DeliveryEntry> {
    let result = match operation {
        Ok(result) => result,
        Err(_) => {
            // Loading waits behind an outstanding blocking write and obtains its durable revision.
            entry = journal.load(id).await?;
            Err(DeliveryProblem::Transport.error())
        }
    };
    if let Err(error) = result {
        let problem = match error.detail {
            Some(culprit_core::ErrorDetail::Delivery(problem)) => problem,
            _ => DeliveryProblem::Transport,
        };
        entry.state.problem = Some(problem);
        entry.state.retry_at_ms = now.saturating_add(i64::from(policy.retry_delay_ms));
        if matches!(
            problem,
            DeliveryProblem::Integrity
                | DeliveryProblem::InvalidContract
                | DeliveryProblem::DestinationChanged
                | DeliveryProblem::UnsupportedSchema
        ) {
            entry.state.phase = DeliveryPhase::Quarantined;
        }
        if problem == DeliveryProblem::Conflict {
            entry.state.head_known = false;
        }
        journal.save(&mut entry).await?;
        return Err(problem.error());
    }
    Ok(entry.status())
}
