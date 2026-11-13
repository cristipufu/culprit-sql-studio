//! Request-memory investigation consumes only one authenticated database observation and its native generations.
use super::*;
use culprit_core::{DatabaseIdentity, collected_resource_analysis::CollectedMemoryParticipantsAnalysis};

pub fn request_memory(capture: &EvidenceCapture) -> Result<CollectedMemoryParticipantsAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::CurrentRequestMemory)?;
    let database = input.database()?;
    let bounds = input.bounds()?;
    let mut observed_point = None;
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::ExecutionGrant(grant) = &row.fact else { return Err(invalid()) };
            let point = qualify(grant, database, &bounds)?;
            if observed_point.as_ref().is_some_and(|previous| previous != &point) {
                return Err(invalid());
            }
            observed_point = Some(point);
            Ok(grant.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = input.outcome(observations.len())?;
    let report = culprit_checks::request_memory::explain(&observations, Some(&source))?;
    Ok(CollectedMemoryParticipantsAnalysis { context: crate::collected_context::from_capture(capture), report })
}

fn qualify(
    observation: &ExecutionGrantObservation,
    database: &DatabaseIdentity,
    bounds: &ObservationBounds,
) -> Result<String> {
    if !bounds.contains(&observation.observed_at) {
        return Err(invalid());
    }
    let observed_at = canonical_time(&observation.observed_at).map_err(|_| invalid())?;
    if !observation.fits_database_capture(&database.database) {
        return Err(invalid());
    }
    Ok(observed_at)
}
