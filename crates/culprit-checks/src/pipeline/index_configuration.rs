//! Current source families and authenticated originals remain independent; no join uses current labels.
use super::*;
pub(super) fn explain(snapshot: &mut Snapshot) -> Vec<culprit_core::IndexConfigurationReport> {
    let mut reports = Vec::new();
    for (dataset, source) in [
        (culprit_core::telemetry::SnapshotDataset::Indexes, culprit_core::source_id::INDEXES),
        (culprit_core::telemetry::SnapshotDataset::Tables, culprit_core::source_id::TABLES),
    ] {
        match crate::index_configuration::explain_snapshot(snapshot, dataset) {
            Ok(Some(report)) => reports.push(report),
            Ok(None) => {}
            Err(error) => {
                let _ = admit_explanation(
                    snapshot,
                    source.into(),
                    culprit_core::message!("view.index_configuration.invalid"),
                    Err::<(), _>(error),
                );
            }
        }
    }
    if let Some(originals) = super::recorded_operational::explain(
        &snapshot.operations.index_configuration,
        crate::index_configuration::explain_recorded,
        crate::index_configuration::unavailable,
        culprit_core::message!("view.index_configuration.invalid"),
        |shown, total| {
            culprit_core::message!("view.index_configuration.presentation_limited", shown = shown, total = total)
        },
    ) {
        reports.extend(originals);
    }
    reports
}
