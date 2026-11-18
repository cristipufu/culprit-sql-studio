use super::*;
use culprit_core::collected_resource_analysis::CollectedInstanceResourceAnalysis;

pub fn instance_resources(capture: &EvidenceCapture) -> Result<CollectedInstanceResourceAnalysis> {
    let input = CapturedState::sources(capture, SnapshotDataset::Operational)?
        .into_iter()
        .find(|input| input.coverage.source == INSTANCE_RESOURCES_SOURCE)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    input.shared(&[SharedResourceKind::Instance])?;
    let rows = input.rows()?.into_iter().cloned().collect::<Vec<_>>();
    let read = InstanceResourceRead::from_source(capture.source(), &rows, capture.is_read_complete())
        .map_err(|_| invalid())?;
    let report = culprit_checks::instance_resources::explain(&read).map_err(|_| invalid())?;
    Ok(CollectedInstanceResourceAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
