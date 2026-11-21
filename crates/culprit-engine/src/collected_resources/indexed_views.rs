//! The selected original Indexes source is interpreted without native reads or a current-catalog name join.
use super::*;
use culprit_core::collected_resource_analysis::CollectedIndexedViewsAnalysis;
pub fn indexed_views(capture: &EvidenceCapture) -> Result<CollectedIndexedViewsAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::Indexes)?;
    let rows = input.rows()?;
    let source = read_outcome(input.coverage, rows.len(), capture.is_read_complete())?;
    let report = culprit_checks::indexed_views::explain(culprit_checks::table_catalog::MssqlCatalogEvidence {
        description: capture.source(),
        observations: &rows,
        source: &source,
        read_complete: capture.is_read_complete(),
    })?;
    Ok(CollectedIndexedViewsAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
