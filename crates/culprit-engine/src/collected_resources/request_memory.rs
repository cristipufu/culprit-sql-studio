//! Request-memory investigation consumes only one authenticated database observation and its native generations.
use super::*;
use culprit_core::collected_resource_analysis::CollectedMemoryParticipantsAnalysis;

pub fn request_memory(capture: &EvidenceCapture) -> Result<CollectedMemoryParticipantsAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::CurrentRequestMemory)?;
    input.database()?;
    let context = NativeReadContext::from_snapshot(capture.source()).map_err(|_| invalid())?;
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::ExecutionGrant(grant) = &row.fact else { return Err(invalid()) };
            Ok(grant.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = input.outcome(observations.len())?;
    let report = culprit_checks::request_memory::explain(
        culprit_checks::request_memory::RequestMemoryEvidence { context: &context, observations: &observations },
        Some(&source),
    )
    .map_err(|_| invalid())?;
    Ok(CollectedMemoryParticipantsAnalysis { context: crate::collected_context::from_capture(capture), report })
}
