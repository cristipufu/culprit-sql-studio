//! Native reported holders retain point relationships, independent of writer plans or contention contribution.
use super::*;
use culprit_core::collected_resource_analysis::CollectedPageTaskHolderAnalysis;

pub fn page_task_holders(capture: &EvidenceCapture) -> Result<CollectedPageTaskHolderAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::PageTaskHolders)?;
    let SourceObservation::PageTaskHolders { focus, .. } = &capture.source().observation else {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    };
    if !capture.source().partition_is_consistent() || !capture.source().observation.covers_request() {
        return Err(invalid());
    }
    let row = input.single_row()?;
    let context = NativeReadContext::from_snapshot(capture.source()).map_err(|_| invalid())?;
    let observation = row
        .map(|row| {
            let OperationalFact::PageTaskHolders(point) = &row.fact else { return Err(invalid()) };
            if !focus.accepts_observation(&context, point) {
                return Err(invalid());
            }
            Ok(point.as_ref())
        })
        .transpose()?;
    let source = input.outcome(usize::from(row.is_some()))?;
    let report = culprit_checks::page_waits::explain_task_holders(focus, &context, observation, Some(&source))?;
    Ok(CollectedPageTaskHolderAnalysis { context: crate::collected_context::from_capture(capture), report })
}
