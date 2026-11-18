use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::HyperscaleLogComponentReport>> {
    match crate::hyperscale_log_components::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(error) => admit_explanation(
            snapshot,
            culprit_core::telemetry::HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
            culprit_core::message!("view.hyperscale_log_components.invalid"),
            Err(error),
        ),
    }
}
