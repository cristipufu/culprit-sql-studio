//! A current scan supplies no ownership, continuity or runtime completion for these independent originals.
use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Vec<culprit_core::FullTextPopulationReport>> {
    super::recorded_operational::explain(
        &mut snapshot.operations.full_text_populations,
        crate::full_text_populations::explain_recorded,
        crate::full_text_populations::unavailable,
        culprit_core::message!("view.full_text_populations.invalid"),
        |shown, total| {
            culprit_core::message!(
                "view.full_text_populations.presentation_limited",
                shown = culprit_core::format::digits(shown.to_string()),
                total = culprit_core::format::digits(total.to_string())
            )
        },
    )
}
