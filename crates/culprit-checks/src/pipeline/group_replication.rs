//! A later scan supplies no database/member/table continuity for independently authenticated originals.
use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Vec<culprit_core::GroupReplicationReport>> {
    super::recorded_operational::explain(
        &snapshot.operations.group_replication,
        crate::group_replication::explain_recorded,
        crate::group_replication::unavailable,
        culprit_core::message!("view.group_replication.invalid"),
        |shown, total| {
            culprit_core::message!(
                "view.group_replication.presentation_limited",
                shown = culprit_core::format::digits(shown.to_string()),
                total = culprit_core::format::digits(total.to_string())
            )
        },
    )
}
