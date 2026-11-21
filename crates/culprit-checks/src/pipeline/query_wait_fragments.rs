use super::*;

pub(super) fn explain(snapshot: &Snapshot) -> Option<Box<culprit_core::query_wait_report::QueryWaitFragmentReport>> {
    match crate::query_wait_fragments::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(_) => Some(Box::new(crate::query_wait_fragments::unavailable(&culprit_core::message!(
            "view.query_wait_fragments.invalid"
        )))),
    }
}
