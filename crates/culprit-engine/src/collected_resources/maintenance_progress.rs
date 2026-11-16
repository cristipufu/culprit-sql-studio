//! The immutable native result owns its original participant and roster even when warehouse rows are missing.
use super::*;
use culprit_core::collected_resource_analysis::CollectedMaintenanceProgressAnalysis;

pub fn maintenance_progress(capture: &EvidenceCapture) -> Result<CollectedMaintenanceProgressAnalysis> {
    let SourceObservation::MaintenanceProgress { snapshot: native, result } = &capture.source().observation else {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    };
    if !result.is_valid_source(capture.source(), native) || !capture.source().partition_is_consistent() {
        return Err(invalid());
    }
    let input = CapturedState::new(capture, SnapshotDataset::MaintenanceProgress)?;
    input.database()?;
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::MaintenanceProgress(point) = &row.fact else { return Err(invalid()) };
            if point.key()? != row.key || !point.fits_source_scope(capture.source()) {
                return Err(invalid());
            }
            Ok(point.as_ref().clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let outcome = input.outcome(observations.len())?;
    let snapshot = MaintenanceProgressSnapshot {
        source: input.coverage.source.clone(),
        context: result.read_context(capture.source(), native),
        coverage: input.coverage.clone(),
        result: result.as_ref().clone(),
        observations,
    };
    let mut report =
        culprit_checks::maintenance_progress::explain(&snapshot, capture.is_read_complete()).map_err(|_| invalid())?;
    report.source_state = outcome.state;
    report.source_reason = outcome.reason;
    Ok(CollectedMaintenanceProgressAnalysis { context: crate::collected_context::from_capture(capture), report })
}
