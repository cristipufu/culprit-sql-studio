//! Capture-local page proof remains independent from later requester/wait changes. Original waiters are not owners.
use super::*;
use culprit_core::collected_resource_analysis::CollectedPageWaitAnalysis;

pub fn page_waits(capture: &EvidenceCapture) -> Result<CollectedPageWaitAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::PageWaitTargets)?;
    if let SourceObservation::FocusedPageWait { result, .. } = &capture.source().observation
        && !result.focus.matches_capture_scope(capture.source())
    {
        return Err(invalid());
    }
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::PageWait(observation) = &row.fact else { return Err(invalid()) };
            // The authenticated source binds shared authority, original request scope and native bounds together.
            if !observation.fits_source_scope(capture.source()) {
                return Err(invalid());
            }
            if let SourceObservation::FocusedPageWait { result, .. } = &capture.source().observation
                && !result.accepts_observation(observation)
            {
                return Err(invalid());
            }
            Ok(observation.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = input.outcome(observations.len())?;
    let report = culprit_checks::page_waits::explain(&observations, Some(&source))?;
    Ok(CollectedPageWaitAnalysis { context: crate::collected_context::from_capture(capture), report })
}
