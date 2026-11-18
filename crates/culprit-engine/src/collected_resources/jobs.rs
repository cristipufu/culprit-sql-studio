//! Jobs use the original native descriptor and retained rows; no current definition or worker is consulted.
use super::*;
use culprit_core::collected_resource_analysis::CollectedJobsAnalysis;
pub fn jobs(capture: &EvidenceCapture) -> Result<CollectedJobsAnalysis> {
    let inputs = CapturedState::sources(capture, SnapshotDataset::Jobs)?;
    let mut rows = Vec::new();
    for input in inputs {
        rows.extend(input.rows()?.into_iter().cloned());
    }
    let snapshot =
        JobsSnapshot::from_source(capture.source(), rows, capture.is_read_complete()).map_err(|_| invalid())?;
    let report = culprit_checks::jobs::explain(&snapshot).map_err(|_| invalid())?;
    Ok(CollectedJobsAnalysis { context: crate::collected_context::from_capture(capture), report })
}
