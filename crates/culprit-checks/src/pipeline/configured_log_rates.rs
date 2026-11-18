use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::ConfiguredLogRateReport>> {
    snapshot.pressure.as_ref()?.configured_log_rates.as_ref()?;
    let result = crate::configured_log_rates::explain_snapshot(snapshot).and_then(|report| {
        report.map(Box::new).ok_or_else(|| culprit_core::Error::invalid(t!("view.configured_log_rates.invalid")))
    });
    admit_explanation(
        snapshot,
        culprit_core::telemetry::CONFIGURED_LOG_RATES_SOURCE.into(),
        culprit_core::message!("view.configured_log_rates.invalid"),
        result,
    )
}
