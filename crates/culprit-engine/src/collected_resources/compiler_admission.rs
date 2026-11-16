//! Compiler admission readback preserves raw request and gateway receipts without a workload or pool ownership join.
use super::*;
use culprit_core::collected_resource_analysis::CollectedCompilerAdmissionAnalysis;

pub fn compiler_admission(capture: &EvidenceCapture) -> Result<CollectedCompilerAdmissionAnalysis> {
    let dataset =
        capture.source().observation.snapshot().ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?.dataset;
    if !matches!(dataset, SnapshotDataset::CompilerAdmission | SnapshotDataset::Operational) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let sources = CapturedState::sources(capture, dataset)?;
    let input = sources
        .into_iter()
        .find(|source| source.coverage.source == culprit_core::source_id::COMPILATION_GATEWAYS)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    let rows = input.rows()?;
    let source = input.outcome(rows.len())?;
    let snapshot = CompilerAdmissionSnapshot {
        source: input.coverage.source.clone(),
        context: Some(NativeReadContext::from_snapshot(capture.source()).map_err(|_| invalid())?),
        observations: rows.into_iter().cloned().collect(),
    };
    if let SourceObservation::FocusedCompilationAdmission { result, .. } = &capture.source().observation
        && (!result.focus.matches_capture_scope(capture.source())
            || snapshot.observations.iter().any(|row| !result.accepts_observation(row))
            || capture.is_read_complete() && !result.matches_observations(&snapshot.observations))
    {
        return Err(invalid());
    }
    let report = culprit_checks::compiler_admission::explain(&snapshot, Some(&source)).map_err(|_| invalid())?;
    Ok(CollectedCompilerAdmissionAnalysis { context: crate::collected_context::from_capture(capture), report })
}
