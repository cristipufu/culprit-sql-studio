//! Restore and authenticate original writer prerequisites before analyzing a later native result.
use super::*;

pub(super) async fn verify_parents(
    outbox: &mut Outbox,
    connection: &str,
    focus: &PageWaitWriterFocus,
    recovery: Option<&dyn RecoveryStore>,
    cancellation: &CollectionCancellation,
) -> Result<bool> {
    let Some((source, row)) = resolve_evidence(outbox, connection, &focus.page, recovery, cancellation).await? else {
        return Ok(false);
    };
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) =
        &row.record
    else {
        return Err(DeliveryProblem::Integrity.error());
    };
    if !focus.matches_page(&source, page, &focus.page) {
        return Err(DeliveryProblem::Integrity.error());
    }
    let parent = outbox
        .diagnostic_input(&focus.waiter.participant.parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await?;
    if !focus.matches_activity_frame(&parent.activity_frame()?) {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(true)
}
