//! Writer access plans remain original captured facts, separate from latch ownership or page-level causation.
use super::*;
use culprit_core::collected_resource_analysis::CollectedPageWaitWriterAnalysis;

pub fn page_wait_writers(capture: &EvidenceCapture) -> Result<CollectedPageWaitWriterAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::PageWaitWriters)?;
    let SourceObservation::PageWaitWriters { focus, .. } = &capture.source().observation else {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    };
    if !capture.source().partition_is_consistent() || !capture.source().observation.covers_request() {
        return Err(invalid());
    }
    let row = input.single_row()?;
    let context = NativeReadContext::from_snapshot(capture.source()).map_err(|_| invalid())?;
    let observation = row
        .map(|row| {
            let OperationalFact::PageWaitWriters(observation) = &row.fact else { return Err(invalid()) };
            if !focus.accepts_observation(&context, observation) {
                return Err(invalid());
            }
            Ok(observation.as_ref())
        })
        .transpose()?;
    let source = input.outcome(usize::from(row.is_some()))?;
    let report = culprit_checks::page_waits::explain_writers(focus, &context, observation, Some(&source))?;
    Ok(CollectedPageWaitWriterAnalysis { context: crate::collected_context::from_capture(capture), report })
}
