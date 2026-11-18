//! A retained Operational point uses original source coverage; no source or recovery read is performed here.
use super::*;
use culprit_core::collected_resource_analysis::CollectedConfiguredLogRateAnalysis;

pub fn configured_log_rates(capture: &EvidenceCapture) -> Result<CollectedConfiguredLogRateAnalysis> {
    let input = CapturedState::sources(capture, SnapshotDataset::Operational)?
        .into_iter()
        .find(|input| input.coverage.source == CONFIGURED_LOG_RATES_SOURCE)
        .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())?;
    let row = input.single_row()?;
    let point = row
        .map(|row| match &row.fact {
            OperationalFact::ConfiguredLogRateLimits(point) => Ok(point.as_ref().clone()),
            _ => Err(invalid()),
        })
        .transpose()?;
    let read = ConfiguredLogRateRead::from_source(capture.source(), point).map_err(|_| invalid())?;
    let report =
        culprit_checks::configured_log_rates::explain(&read, capture.is_read_complete()).map_err(|_| invalid())?;
    Ok(CollectedConfiguredLogRateAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
