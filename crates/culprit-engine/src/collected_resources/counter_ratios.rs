//! Counter fractions reuse original authenticated sets and source clocks, without source access or hydration.
use super::*;
use culprit_core::collected_resource_analysis::CollectedCounterRatioAnalysis;

pub fn counter_ratios(capture: &EvidenceCapture) -> Result<CollectedCounterRatioAnalysis> {
    let sources = CapturedState::sources(capture, SnapshotDataset::Operational)?;
    let input = sources
        .into_iter()
        .find(|source| source.coverage.source == culprit_core::source_id::PERFORMANCE_COUNTERS)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    let rows = input.rows()?;
    let bounds = input.bounds()?;
    let samples = rows
        .iter()
        .map(|row| {
            let OperationalFact::CounterSet(sample) = &row.fact else {
                return Err(invalid());
            };
            if sample.ratios.iter().any(|ratio| ratio.observed_at.as_deref().is_some_and(|at| !bounds.contains(at))) {
                return Err(invalid());
            }
            Ok(sample.as_ref().clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let outcome = input.outcome(rows.len())?;
    let report = culprit_checks::counter_ratios::explain(&samples, Some(&outcome), capture.is_read_complete())
        .map_err(|_| invalid())?;
    Ok(CollectedCounterRatioAnalysis { context: crate::collected_context::from_capture(capture), report })
}
