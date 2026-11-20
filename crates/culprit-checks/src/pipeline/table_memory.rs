//! A later scan supplies no database/member/table continuity for independently authenticated originals.
use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Vec<culprit_core::TableMemoryReport>> {
    super::recorded_operational::explain(
        &mut snapshot.operations.table_memory,
        crate::table_memory::explain_recorded,
        crate::table_memory::unavailable,
        culprit_core::message!("view.table_memory.invalid"),
        |shown, total| {
            culprit_core::message!(
                "view.table_memory.presentation_limited",
                shown = culprit_core::format::digits(shown.to_string()),
                total = culprit_core::format::digits(total.to_string())
            )
        },
    )
}
