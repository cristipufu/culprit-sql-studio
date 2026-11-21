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
    let report = explain_read(capture.source(), rows, capture.is_read_complete())?;
    Ok(CollectedReplicationAnalysis { context: crate::collected_context::from_capture(capture), report })
}

/// The same original-source interpretation serves local authenticated files and warehouse captures.
pub(crate) fn explain_read(
    source: &CollectionDescription,
    rows: Vec<ReplicationObservation>,
    read_complete: bool,
) -> Result<culprit_core::ReplicationReport> {
    let snapshot = ReplicationSnapshot::from_source(source, rows).map_err(|_| invalid())?;
    culprit_checks::replication::explain(&snapshot, read_complete).map_err(|_| invalid())
}
