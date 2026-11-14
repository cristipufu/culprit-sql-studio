//! Cleanup explanations use one authenticated capture and the same pure view as native scans.
use super::*;
use culprit_core::{collected_resource_analysis::CollectedCleanupAnalysis, source_id};

pub fn cleanup(capture: &EvidenceCapture) -> Result<CollectedCleanupAnalysis> {
    let SourceObservation::Snapshot(native) = &capture.source().observation else {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    };
    if !matches!(native.dataset, SnapshotDataset::Cleanup | SnapshotDataset::Operational) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let mut observations = Vec::new();
    let mut sources = Vec::new();
    for input in CapturedState::sources(capture, native.dataset)? {
        let database = input.database()?;
        let bounds = input.bounds()?;
        let rows = input.rows()?;
        match native.dataset {
            SnapshotDataset::Cleanup => {
                for row in &rows {
                    match &row.fact {
                        OperationalFact::Retention(holder)
                            if bounds.contains(&holder.observed_at)
                                && holder.fits_database_capture(&database.database) => {}
                        OperationalFact::RetentionCoverage(_) => (),
                        _ => return Err(invalid()),
                    }
                }
                sources.push(input.outcome(rows.len())?);
                observations.extend(rows);
            }
            SnapshotDataset::Operational => {
                let selected = rows.iter().copied().filter(|row| is_native_cleanup(&row.fact)).collect::<Vec<_>>();
                let declared_cleanup = matches!(
                    input.coverage.source.as_str(),
                    source_id::PERSISTENT_VERSION_STORE | source_id::OPEN_TRANSACTIONS
                );
                if declared_cleanup || !selected.is_empty() {
                    if declared_cleanup && selected.len() != rows.len() {
                        return Err(invalid());
                    }
                    sources.push(input.outcome(rows.len())?);
                    observations.extend(selected);
                }
            }
            _ => unreachable!("dataset admitted before reading rows"),
        }
    }
    if sources.is_empty() {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let report = culprit_checks::cleanup::explain(
        culprit_checks::cleanup::CleanupEvidence { retention: None, native: &observations },
        &sources,
    )
    .map_err(|_| invalid())?;
    Ok(CollectedCleanupAnalysis { context: crate::collected_context::from_capture(capture), report })
}

fn is_native_cleanup(fact: &OperationalFact) -> bool {
    matches!(fact, OperationalFact::MssqlPersistentVersionStore(_) | OperationalFact::MssqlCleanupHorizon(_))
}
