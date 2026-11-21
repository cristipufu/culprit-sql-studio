//! Reuse the original OperationalSamples read and pure interpreter; never query current catalog metadata.
use super::*;
use culprit_core::collected_resource_analysis::CollectedGaleraAnalysis;
pub fn galera(capture: &EvidenceCapture) -> Result<CollectedGaleraAnalysis> {
    let inputs = CapturedState::sources(capture, SnapshotDataset::Galera)?;
    let mut rows = Vec::new();
    for input in inputs {
        rows.extend(input.rows()?);
    }
    let report = culprit_checks::galera::explain(culprit_checks::galera::GaleraEvidence {
        source: capture.source(),
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })?;
    Ok(CollectedGaleraAnalysis { context: crate::collected_context::from_capture(capture), report: Box::new(report) })
}
