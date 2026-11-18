//! One authenticated capture keeps original clerk coverage, server scope and independent warehouse body gaps.
use super::*;
use culprit_checks::memory_clerks::MemoryClerkEvidence;
use culprit_core::collected_resource_analysis::CollectedMemoryClerkAnalysis;

pub fn memory_clerks(capture: &EvidenceCapture) -> Result<CollectedMemoryClerkAnalysis> {
    let input = CapturedState::sources(capture, SnapshotDataset::Operational)?
        .into_iter()
        .find(|input| input.coverage.source == culprit_core::source_id::MEMORY_CLERKS)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    let context = NativeReadContext::from_snapshot(capture.source()).map_err(|_| invalid())?;
    let rows = input.rows()?;
    let report = culprit_checks::memory_clerks::explain(MemoryClerkEvidence::Captured {
        context: &context,
        coverage: input.coverage,
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })
    .map_err(|_| invalid())?;
    Ok(CollectedMemoryClerkAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
