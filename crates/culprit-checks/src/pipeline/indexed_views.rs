//! Current catalog disclosure and independently authenticated originals remain separate.
use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Vec<culprit_core::IndexedViewsReport> {
    let mut reports = Vec::new();
    match crate::indexed_views::explain_snapshot(snapshot) {
        Ok(Some(report)) => reports.push(report),
        Ok(None) => {}
        Err(error) => {
            let _ = admit_explanation(
                snapshot,
                culprit_core::source_id::INDEXES.into(),
                culprit_core::message!("view.indexed_views.invalid"),
                Err::<(), _>(error),
            );
            reports.push(crate::indexed_views::unavailable(&culprit_core::message!("view.indexed_views.invalid")));
        }
    }
    if let Some(originals) = super::recorded_operational::explain(
        &snapshot.operations.indexed_view_originals,
        crate::indexed_views::explain_recorded,
        crate::indexed_views::unavailable,
        culprit_core::message!("view.indexed_views.invalid"),
        |shown, total| culprit_core::message!("view.indexed_views.presentation_limited", shown = shown, total = total),
    ) {
        reports.extend(originals);
    }
    reports
}
