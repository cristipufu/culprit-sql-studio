use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::JobsReport>> {
    let source = snapshot.operations.jobs.as_ref()?.primary_source();
    let result = crate::jobs::explain_snapshot(snapshot)
        .and_then(|report| report.map(Box::new).ok_or_else(|| culprit_core::Error::invalid(t!("view.jobs.invalid"))));
    admit_explanation(snapshot, source.into(), culprit_core::message!("view.jobs.invalid"), result)
}
