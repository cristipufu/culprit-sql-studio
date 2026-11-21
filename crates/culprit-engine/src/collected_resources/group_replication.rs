//! Reuse the original OperationalSamples read and pure interpreter; never query current catalog metadata.
use super::*;
use culprit_core::collected_resource_analysis::CollectedGroupReplicationAnalysis;
pub fn group_replication(capture: &EvidenceCapture) -> Result<CollectedGroupReplicationAnalysis> {
    let inputs = CapturedState::sources(capture, SnapshotDataset::GroupReplication)?;
    let mut rows = Vec::new();
    for input in inputs {
        rows.extend(input.rows()?);
    }
    let report =
        culprit_checks::group_replication::explain(culprit_checks::group_replication::GroupReplicationEvidence {
            source: capture.source(),
            observations: &rows,
            read_complete: capture.is_read_complete(),
        })?;
    Ok(CollectedGroupReplicationAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
