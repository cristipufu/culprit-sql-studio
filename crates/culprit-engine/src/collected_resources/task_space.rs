//! Authenticated task counters reuse the same explanation as an explicit scan, without a synthetic workload.
use super::*;
use culprit_core::collected_resource_analysis::CollectedTaskSpaceAnalysis;

pub fn task_space(capture: &EvidenceCapture) -> Result<CollectedTaskSpaceAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::TaskSpace)?;
    let database = input.database()?;
    let bounds = input.bounds()?;
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::TaskSpace(observation) = &row.fact else { return Err(invalid()) };
            if !bounds.contains(&observation.observed_at)
                || [&observation.before_check, &observation.after_check]
                    .into_iter()
                    .flatten()
                    .any(|check| !bounds.contains(&check.observed_at))
                || !observation.fits_database_capture(&database.database)
            {
                return Err(invalid());
            }
            Ok(observation.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = input.outcome(observations.len())?;
    let report = culprit_checks::task_space::explain(&observations, Some(&source))?;
    Ok(CollectedTaskSpaceAnalysis { context: crate::collected_context::from_capture(capture), report })
}
