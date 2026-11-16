//! Original replication captures use the same pure explanation as scans, without reopening a provider.
use super::*;
use culprit_core::collected_resource_analysis::CollectedReplicationAnalysis;

pub fn replication(capture: &EvidenceCapture) -> Result<CollectedReplicationAnalysis> {
    let inputs = CapturedState::sources_for(capture, SnapshotDataset::Replication, EvidenceFamily::ReplicationSamples)?;
    let mut rows = Vec::new();
    for input in inputs {
        let selected = input.replication()?;
        rows.extend(selected.into_iter().cloned());
    }
    let snapshot = ReplicationSnapshot::from_source(capture.source(), rows).map_err(|_| invalid())?;
    let report = culprit_checks::replication::explain(&snapshot, capture.is_read_complete()).map_err(|_| invalid())?;
    Ok(CollectedReplicationAnalysis { context: crate::collected_context::from_capture(capture), report })
}
