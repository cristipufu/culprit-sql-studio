//! Explain the exact original cached-entry rows; no workload, current request or live lookup is synthesized.
use super::*;
use culprit_core::collected_resource_analysis::CollectedStatementGrantAnalysis;

pub fn statement_grants(capture: &EvidenceCapture) -> Result<CollectedStatementGrantAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::StatementGrants)?;
    input.database()?;
    input.bounds()?;
    let observations = input
        .rows()?
        .into_iter()
        .map(|observation| {
            let OperationalFact::MssqlStatementGrants(grant) = &observation.fact else { return Err(invalid()) };
            Ok(grant.as_ref().clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = input.outcome(observations.len())?;
    let snapshot = StatementGrantSnapshot {
        capture: capture.capture().clone(),
        description: capture.source().clone(),
        observations,
    };
    let report = culprit_checks::statement_grants::explain_read(&[snapshot], &[source], capture.is_read_complete())
        .map_err(|_| invalid())?;
    Ok(CollectedStatementGrantAnalysis { context: crate::collected_context::from_capture(capture), report })
}
