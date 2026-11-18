//! The authenticated original Operational capture owns coverage and clocks; no live read or hydration occurs.
use super::*;
use culprit_checks::performance_counters::PerformanceCounterEvidence;
use culprit_core::collected_resource_analysis::CollectedPerformanceCounterAnalysis;

pub fn performance_counters(capture: &EvidenceCapture) -> Result<CollectedPerformanceCounterAnalysis> {
    let input = CapturedState::sources(capture, SnapshotDataset::Operational)?
        .into_iter()
        .find(|input| input.coverage.source == culprit_core::source_id::PERFORMANCE_COUNTERS)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    let context = NativeReadContext::from_snapshot(capture.source()).map_err(|_| invalid())?;
    let rows = input.rows()?;
    let report = culprit_checks::performance_counters::explain(PerformanceCounterEvidence::Captured {
        context: &context,
        coverage: input.coverage,
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })
    .map_err(|_| invalid())?;
    Ok(CollectedPerformanceCounterAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
