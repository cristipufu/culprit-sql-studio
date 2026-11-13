//! Capture-local page proof remains independent from later requester/wait changes. Original waiters are not owners.
use super::*;
use culprit_core::collected_resource_analysis::CollectedPageWaitAnalysis;

pub fn page_waits(capture: &EvidenceCapture) -> Result<CollectedPageWaitAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::PageWaitTargets)?;
    let database = input.database()?;
    let bounds = input.bounds()?;
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::PageWait(observation) = &row.fact else { return Err(invalid()) };
            // Native validation owns recheck validity/order within these bounds and the exact checked-reference policy.
            if !bounds.contains(&observation.observed_from) || !bounds.contains(&observation.observed_until) {
                return Err(invalid());
            }
            if !observation.fits_database_capture(&database.database) {
                return Err(invalid());
            }
            Ok(observation.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = input.outcome(observations.len())?;
    let report = culprit_checks::page_waits::explain(&observations, Some(&source))?;
    Ok(CollectedPageWaitAnalysis { context: crate::collected_context::from_capture(capture), report })
}
