//! Inspect admitted native statement counter points, without synthesizing query history or a Snapshot.
use super::*;
use culprit_core::collected_resource_analysis::CollectedStatementWorkAnalysis;

pub fn statement_work(capture: &EvidenceCapture) -> Result<CollectedStatementWorkAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::StatementCounters)?;
    input.database()?;
    let observations = input.rows()?;
    let snapshot = culprit_core::telemetry::StatementCounterSnapshot::from_source(
        capture.source(),
        observations.into_iter().cloned().collect(),
    )
    .map_err(|_| invalid())?;
    let report =
        culprit_checks::statement_work::explain(&snapshot, capture.is_read_complete()).map_err(|_| invalid())?;
    Ok(CollectedStatementWorkAnalysis { context: crate::collected_context::from_capture(capture), report })
}
