//! The authenticated original Operational capture owns coverage and clocks; no live read or hydration occurs.
use super::*;
use culprit_core::collected_resource_analysis::CollectedPerformanceCounterAnalysis;

pub fn performance_counters(capture: &EvidenceCapture) -> Result<CollectedPerformanceCounterAnalysis> {
    let dataset = capture.source().observation.snapshot().ok_or_else(invalid)?.dataset;
    if !matches!(dataset, SnapshotDataset::Operational | SnapshotDataset::DatabasePerformanceCounters) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let input = CapturedState::sources(capture, dataset)?
        .into_iter()
        .find(|input| input.coverage.source == culprit_core::source_id::PERFORMANCE_COUNTERS)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    let rows = input.rows()?;
    let report = culprit_checks::performance_counters::explain_source(
        capture.source(),
        input.coverage,
        &rows,
        capture.is_read_complete(),
    )
    .map_err(|_| invalid())?;
    Ok(CollectedPerformanceCounterAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
