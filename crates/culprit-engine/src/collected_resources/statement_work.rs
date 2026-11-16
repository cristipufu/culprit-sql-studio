//! Inspect admitted native statement counter points, without synthesizing query history or a Snapshot.
use super::*;
use culprit_core::collected_resource_analysis::CollectedStatementWorkAnalysis;

pub fn statement_work(capture: &EvidenceCapture) -> Result<CollectedStatementWorkAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::StatementCounters)?;
    input.database()?;
    let context = NativeReadContext::from_snapshot(capture.source()).map_err(|_| invalid())?;
    let observations = input.rows()?;
    let source = input.outcome(observations.len())?;
    let report =
        culprit_checks::statement_work::explain(&context, &observations, Some(&source)).map_err(|_| invalid())?;
    Ok(CollectedStatementWorkAnalysis { context: crate::collected_context::from_capture(capture), report })
}
