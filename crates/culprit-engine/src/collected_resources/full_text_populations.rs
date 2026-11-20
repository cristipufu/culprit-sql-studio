//! Exact authenticated original rows, interpreted once without another database read or implicit recovery.
use super::*;
use culprit_core::collected_resource_analysis::CollectedFullTextPopulationAnalysis;

pub fn full_text_populations(capture: &EvidenceCapture) -> Result<CollectedFullTextPopulationAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::FullTextPopulations)?;
    let rows = input.rows()?;
    let report = culprit_checks::full_text_populations::explain(
        culprit_checks::full_text_populations::FullTextPopulationEvidence {
            source: capture.source(),
            observations: &rows,
            read_complete: capture.is_read_complete(),
        },
    )?;
    Ok(CollectedFullTextPopulationAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
