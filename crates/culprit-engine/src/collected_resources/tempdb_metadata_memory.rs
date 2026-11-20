//! Existing authenticated shared Operational rows enter the same pure tempdb metadata-memory interpretation once.
use super::*;
use culprit_checks::tempdb_metadata_memory::TempdbMetadataMemoryEvidence;
use culprit_core::collected_resource_analysis::CollectedTempdbMetadataMemoryAnalysis;

pub fn tempdb_metadata_memory(capture: &EvidenceCapture) -> Result<CollectedTempdbMetadataMemoryAnalysis> {
    let input = CapturedState::sources(capture, SnapshotDataset::Operational)?
        .into_iter()
        .find(|input| input.coverage.source == culprit_core::source_id::TEMPDB_METADATA_MEMORY)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    let context = NativeReadContext::from_snapshot(capture.source()).map_err(|_| invalid())?;
    let rows = input.rows()?;
    let report = culprit_checks::tempdb_metadata_memory::explain(TempdbMetadataMemoryEvidence {
        context: &context,
        coverage: input.coverage,
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })
    .map_err(|_| invalid())?;
    Ok(CollectedTempdbMetadataMemoryAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
