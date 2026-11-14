//! Reconstruct only exact authenticated population points, with no synthetic workload or cross-capture join.
use super::*;
use culprit_core::collected_resource_analysis::CollectedRuntimeDistributionAnalysis;

pub fn runtime_distributions(capture: &EvidenceCapture) -> Result<CollectedRuntimeDistributionAnalysis> {
    let inputs = CapturedState::sources(capture, SnapshotDataset::RuntimeDistributions)?;
    let mut observations = Vec::new();
    let mut sources = Vec::new();
    for input in inputs {
        input.database()?;
        let bounds = input.bounds()?;
        let rows = input.rows()?;
        for row in &rows {
            let OperationalFact::RuntimeDistribution(point) = &row.fact else { return Err(invalid()) };
            if point.scope != culprit_core::MeasurementScope::Database || !bounds.contains(&point.observed_at) {
                return Err(invalid());
            }
        }
        sources.push(input.outcome(rows.len())?);
        observations.extend(rows);
    }
    let report = culprit_checks::runtime_distributions::explain(&observations, &sources).map_err(|_| invalid())?;
    Ok(CollectedRuntimeDistributionAnalysis { context: crate::collected_context::from_capture(capture), report })
}
