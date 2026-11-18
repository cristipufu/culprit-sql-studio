use super::*;
use culprit_core::collected_resource_analysis::CollectedDatabasePropertiesAnalysis;

pub fn database_properties(capture: &EvidenceCapture) -> Result<CollectedDatabasePropertiesAnalysis> {
    let input = CapturedState::sources(capture, SnapshotDataset::Operational)?
        .into_iter()
        .find(|input| input.coverage.source == DATABASE_PROPERTIES_SOURCE)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    let rows = input.rows()?.into_iter().cloned().collect();
    let read = DatabasePropertiesRead::from_source(capture.source(), rows, capture.is_read_complete())
        .map_err(|_| invalid())?;
    let report = culprit_checks::database_properties::explain(&read).map_err(|_| invalid())?;
    Ok(CollectedDatabasePropertiesAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
