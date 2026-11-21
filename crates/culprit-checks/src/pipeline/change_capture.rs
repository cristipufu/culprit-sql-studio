//! One saved-original replay policy, preserving distinct CDC and change-feed pure reports.
use super::*;
pub(super) fn cdc(snapshot: &Snapshot) -> Option<Vec<culprit_core::CdcReport>> {
    super::recorded_operational::explain(
        &snapshot.operations.cdc_history,
        crate::cdc::explain_recorded,
        crate::cdc::unavailable,
        culprit_core::message!("view.change_capture.history.invalid"),
        overflow,
    )
}
pub(super) fn change_feed(snapshot: &Snapshot) -> Option<Vec<culprit_core::ChangeFeedReport>> {
    super::recorded_operational::explain(
        &snapshot.operations.change_feed_history,
        crate::change_feed::explain_recorded,
        crate::change_feed::unavailable,
        culprit_core::message!("view.change_capture.history.invalid"),
        overflow,
    )
}
fn overflow(shown: usize, total: usize) -> culprit_core::i18n::Message {
    culprit_core::message!(
        "view.change_capture.history.limited",
        shown = culprit_core::format::digits(shown.to_string()),
        total = culprit_core::format::digits(total.to_string())
    )
}
#[cfg(test)]
mod tests;
