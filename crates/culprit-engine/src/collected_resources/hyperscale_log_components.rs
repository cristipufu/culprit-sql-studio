//! The authenticated original source and body feed the same pure explanation as saved snapshots.
use super::*;
use culprit_core::collected_resource_analysis::CollectedHyperscaleLogComponentAnalysis;
pub fn hyperscale_log_components(capture: &EvidenceCapture) -> Result<CollectedHyperscaleLogComponentAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::HyperscaleLogComponents)?;
    let observations = input.rows()?.into_iter().cloned().collect();
    let read = HyperscaleLogComponentRead::from_source(capture.source(), observations, capture.is_read_complete())
        .map_err(|_| invalid())?;
    Ok(CollectedHyperscaleLogComponentAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(culprit_checks::hyperscale_log_components::explain(&read).map_err(|_| invalid())?),
    })
}
