use super::*;
use culprit_core::collected_resource_analysis::CollectedInstanceResourceAnalysis;

pub fn instance_resources(capture: &EvidenceCapture) -> Result<CollectedInstanceResourceAnalysis> {
    let inputs = CapturedState::sources(capture, SnapshotDataset::Operational)?;
    let input = inputs
        .iter()
        .find(|input| input.coverage.source == INSTANCE_RESOURCES_SOURCE)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    input.shared(&[SharedResourceKind::Instance])?;
    let mut rows = Vec::new();
    for input in inputs.iter().filter(|input| {
        matches!(
            input.coverage.source.as_str(),
            INSTANCE_RESOURCES_SOURCE
                | culprit_core::source_id::PROCESSOR_TOPOLOGY
                | culprit_core::source_id::SCHEDULERS
        )
    }) {
        rows.extend(input.rows()?.into_iter().cloned());
    }
    let report =
        culprit_checks::instance_resources::explain_source(capture.source(), &rows, capture.is_read_complete())
            .map_err(|_| invalid())?;
    Ok(CollectedInstanceResourceAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
