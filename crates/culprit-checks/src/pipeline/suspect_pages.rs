use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::SuspectPageHistoryReport>> {
    match crate::suspect_pages::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(error) => admit_explanation(
            snapshot,
            culprit_core::source_id::SUSPECT_PAGES.into(),
            culprit_core::message!("view.suspect_pages.invalid"),
            Err(error),
        ),
    }
}
