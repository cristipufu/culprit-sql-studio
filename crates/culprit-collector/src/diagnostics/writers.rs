//! Paired page questions authenticate their original Activity and page receipts before a later result is used.
use super::*;

pub(super) async fn verify_parents(
    outbox: &mut Outbox,
    connection: &str,
    observation: &SourceObservation,
    recovery: Option<&dyn RecoveryStore>,
    cancellation: &CollectionCancellation,
) -> Result<bool> {
    let (page, parent) = match observation {
        SourceObservation::PageWaitWriters { focus, .. } => (&focus.page, &focus.waiter.participant.parent),
        SourceObservation::PageTaskHolders { focus, .. } => (&focus.page, &focus.participant.parent),
        _ => return Ok(true),
    };
    let Some((source, row)) = resolve_evidence(outbox, connection, page, recovery, cancellation).await? else {
        return Ok(false);
    };
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(point), .. }) =
        &row.record
    else {
        return Err(DeliveryProblem::Integrity.error());
    };
    let Some(original) =
        tasks::resolve_original_activity(outbox, connection, &parent.capture_ref(), recovery, cancellation).await?
    else {
        return Ok(false);
    };
    let frame = original.activity_frame()?;
    let matches = match observation {
        SourceObservation::PageWaitWriters { focus, .. } => {
            focus.matches_page(&source, point, page) && focus.matches_activity_frame(&frame)
        }
        SourceObservation::PageTaskHolders { focus, .. } => {
            focus.matches_page(&source, point, page) && focus.matches_activity_frame(&frame)
        }
        _ => false,
    };
    if !matches {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(true)
}
