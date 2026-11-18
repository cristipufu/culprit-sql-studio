//! Authenticated wait captures reuse the same exact explanation as saved scans, without another source read.
use super::*;
use culprit_core::collected_resource_analysis::CollectedWaitCounterAnalysis;

pub fn wait_counters(capture: &EvidenceCapture) -> Result<CollectedWaitCounterAnalysis> {
    let sources =
        CapturedState::sources_for(capture, SnapshotDataset::WaitCounters, EvidenceFamily::WaitCounterSamples)?;
    let observations = sources
        .iter()
        .map(|source| source.wait_counters())
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let read = WaitCounterSnapshot::from_source(capture.source(), observations, capture.is_read_complete())
        .map_err(|_| invalid())?;
    let report =
        culprit_checks::wait_counters::explain(&read, capture.source().target.engine()).map_err(|_| invalid())?;
    Ok(CollectedWaitCounterAnalysis { context: crate::collected_context::from_capture(capture), report })
}
