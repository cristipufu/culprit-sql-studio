//! Inspect only admitted database statement counter points, without synthesizing query history or a Snapshot.
use super::*;
use culprit_core::collected_resource_analysis::CollectedStatementWorkAnalysis;

pub fn statement_work(capture: &EvidenceCapture) -> Result<CollectedStatementWorkAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::StatementCounters)?;
    input.database()?;
    let bounds = input.bounds()?;
    let observations = input.rows()?;
    for observation in &observations {
        let OperationalFact::StatementCounters(sample) = &observation.fact else { return Err(invalid()) };
        if sample.scope != culprit_core::MeasurementScope::Database || !bounds.contains(&sample.observed_at) {
            return Err(invalid());
        }
    }
    let source = input.outcome(observations.len())?;
    let report = culprit_checks::statement_work::explain(&observations, Some(&source)).map_err(|_| invalid())?;
    Ok(CollectedStatementWorkAnalysis { context: crate::collected_context::from_capture(capture), report })
}
