use super::*;
use culprit_core::collected_resource_analysis::CollectedStorageAnalysis;
pub fn storage(capture: &EvidenceCapture) -> Result<CollectedStorageAnalysis> {
    let inputs = CapturedState::sources(capture, SnapshotDataset::Operational)?;
    if !inputs.iter().any(|input| StorageRead::recognizes(&input.coverage.source)) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let rows = capture
        .records()
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = &record.row().record else {
                return Err(invalid());
            };
            Ok(row.clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let read = StorageRead::from_source(capture.source(), rows, capture.is_read_complete()).map_err(|_| invalid())?;
    Ok(CollectedStorageAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(culprit_checks::storage::explain(&read).map_err(|_| invalid())?),
    })
}
