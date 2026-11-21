use super::*;
pub(super) fn explain(
    snapshot: &Snapshot,
) -> Option<Box<culprit_core::query_runtime_report::QueryRuntimeFragmentReport>> {
    match crate::query_runtime_fragments::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(_) => Some(Box::new(crate::query_runtime_fragments::unavailable(&culprit_core::message!(
            "view.runtime_fragments.invalid"
        )))),
    }
}
