//! Original catalog rows use the same interpreter as scan fields; no live query or inferred name join is performed.
use super::*;
use culprit_core::collected_resource_analysis::CollectedIndexConfigurationAnalysis;
pub fn index_configuration(capture: &EvidenceCapture) -> Result<CollectedIndexConfigurationAnalysis> {
    let dataset = capture.source().observation.snapshot().ok_or_else(invalid)?.dataset;
    if !matches!(dataset, SnapshotDataset::Indexes | SnapshotDataset::Tables) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let input = CapturedState::new(capture, dataset)?;
    let rows = input.rows()?;
    let source = read_outcome(input.coverage, rows.len(), capture.is_read_complete())?;
    let report = culprit_checks::index_configuration::explain(culprit_checks::table_catalog::MssqlCatalogEvidence {
        description: capture.source(),
        observations: &rows,
        source: &source,
        read_complete: capture.is_read_complete(),
    })?;
    Ok(CollectedIndexConfigurationAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
