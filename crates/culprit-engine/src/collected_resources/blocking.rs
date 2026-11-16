//! Reuse original Activity receipts and the same pure graph as current/native explanations.
use super::*;
use culprit_core::collected_resource_analysis::CollectedBlockingAnalysis;

pub fn blocking(capture: &EvidenceCapture) -> Result<CollectedBlockingAnalysis> {
    if !matches!(capture.source().observation, SourceObservation::FocusedBlocking { .. }) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let activity = super::activity::CapturedActivity::new(capture)?;
    let report = crate::explain_blocking(capture.source(), &activity.rows()?, capture.is_read_complete())?;
    Ok(CollectedBlockingAnalysis { context: crate::collected_context::from_capture(capture), report: Box::new(report) })
}
