//! Tracked session allocation evidence belongs to its shared instance, never a current event's database label.
use super::*;
use culprit_core::collected_resource_analysis::CollectedMemoryParticipantsAnalysis;

pub fn session_memory(capture: &EvidenceCapture) -> Result<CollectedMemoryParticipantsAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::SessionMemory)?;
    input.shared(&[SharedResourceKind::Instance])?;
    let bounds = input.bounds()?;
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::SessionMemory(observation) = &row.fact else { return Err(invalid()) };
            if !bounds.contains(&observation.observed_at)
                || observation.current_context.as_ref().is_some_and(|context| !bounds.contains(&context.observed_at))
            {
                return Err(invalid());
            }
            Ok(observation.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = input.outcome(observations.len())?;
    let report = culprit_checks::session_memory::explain(&observations, Some(&source))?;
    Ok(CollectedMemoryParticipantsAnalysis { context: crate::collected_context::from_capture(capture), report })
}
