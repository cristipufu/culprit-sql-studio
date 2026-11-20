//! Reuse the original OperationalSamples read and pure interpreter; never query current catalog metadata.
use super::*;
use culprit_core::collected_resource_analysis::CollectedTableMemoryAnalysis;
pub fn table_memory(capture: &EvidenceCapture) -> Result<CollectedTableMemoryAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::TableMemory)?;
    let rows = input.rows()?;
    let report = culprit_checks::table_memory::explain(culprit_checks::table_memory::TableMemoryEvidence {
        source: capture.source(),
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })?;
    Ok(CollectedTableMemoryAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
