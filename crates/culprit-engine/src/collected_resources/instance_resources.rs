use super::*;
use culprit_core::collected_resource_analysis::CollectedInstanceResourceAnalysis;

pub fn instance_resources(capture: &EvidenceCapture) -> Result<CollectedInstanceResourceAnalysis> {
    let dataset = capture.source().observation.snapshot().ok_or_else(invalid)?.dataset;
    if !matches!(dataset, SnapshotDataset::Operational | SnapshotDataset::NumaNodes) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let inputs = CapturedState::sources(capture, dataset)?;
    if dataset == SnapshotDataset::NumaNodes {
        for input in &inputs {
            input.shared(&[SharedResourceKind::Instance])?;
        }
        let rows = inputs
            .iter()
            .map(|input| input.rows())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let report =
            culprit_checks::instance_resources::explain_source(capture.source(), &rows, capture.is_read_complete())
                .map_err(|_| invalid())?;
        return Ok(CollectedInstanceResourceAnalysis {
            context: crate::collected_context::from_capture(capture),
            report: Box::new(report),
        });
    }
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
