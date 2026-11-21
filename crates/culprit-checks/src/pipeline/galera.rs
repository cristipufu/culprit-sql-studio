//! A later scan supplies no database/member/table continuity for independently authenticated originals.
use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Vec<culprit_core::GaleraReport>> {
    super::recorded_operational::explain(
        &snapshot.operations.galera,
        crate::galera::explain_recorded,
        crate::galera::unavailable,
        culprit_core::message!("view.galera.invalid"),
        |shown, total| {
            culprit_core::message!(
                "view.galera.presentation_limited",
                shown = culprit_core::format::digits(shown.to_string()),
                total = culprit_core::format::digits(total.to_string())
            )
        },
    )
}
