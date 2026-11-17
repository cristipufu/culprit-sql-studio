use super::*;

pub(super) fn explain(
    snapshot: &mut Snapshot,
) -> Option<Box<culprit_core::page_server_read_report::PageServerReadReport>> {
    let result = crate::page_server_reads::explain_snapshot(snapshot).map(|report| report.map(Box::new));
    admit_explanation(
        snapshot,
        culprit_core::source_id::QUERY_STORE.into(),
        culprit_core::message!("view.page_server_reads.invalid"),
        result,
    )
    .flatten()
}
