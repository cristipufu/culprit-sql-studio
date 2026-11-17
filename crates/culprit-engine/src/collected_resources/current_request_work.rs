//! Offline request work uses its original native owner and event binding; no live source fills a readback gap.
use super::*;
use culprit_core::collected_resource_analysis::CollectedCurrentRequestWorkAnalysis;

pub fn current_request_work(capture: &EvidenceCapture) -> Result<CollectedCurrentRequestWorkAnalysis> {
    let SourceObservation::CurrentRequestWork { profile, snapshot: native, result } = &capture.source().observation
    else {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    };
    if !result.is_valid_source(capture.source(), native) || !capture.source().partition_is_consistent() {
        return Err(invalid());
    }
    let input = CapturedState::new(capture, SnapshotDataset::CurrentRequestWork)?;
    input.database()?;
    let observations = input
        .rows()?
        .into_iter()
        .map(|row| {
            let OperationalFact::CurrentRequestWork(point) = &row.fact else { return Err(invalid()) };
            if point.key()? != row.key || !point.fits_source_scope(capture.source()) {
                return Err(invalid());
            }
            Ok(point.as_ref().clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let outcome = input.outcome(observations.len())?;
    let snapshot = CurrentRequestWorkSnapshot {
        profile: *profile,
        source: input.coverage.source.clone(),
        context: result.read_context(capture.source(), native),
        coverage: input.coverage.clone(),
        result: result.as_ref().clone(),
        observations,
    };
    let mut report =
        culprit_checks::current_request_work::explain(&snapshot, capture.is_read_complete()).map_err(|_| invalid())?;
    report.source_state = outcome.state;
    report.source_reason = outcome.reason;
    Ok(CollectedCurrentRequestWorkAnalysis { context: crate::collected_context::from_capture(capture), report })
}
