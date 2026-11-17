use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::JobsReport>> {
    snapshot.operations.jobs.as_ref()?;
    let result = crate::jobs::explain_snapshot(snapshot)
        .and_then(|report| report.map(Box::new).ok_or_else(|| culprit_core::Error::invalid(t!("view.jobs.invalid"))));
    admit_explanation(
        snapshot,
        culprit_core::telemetry::TIMESCALE_JOBS_SOURCE.into(),
        culprit_core::message!("view.jobs.invalid"),
        result,
    )
}
