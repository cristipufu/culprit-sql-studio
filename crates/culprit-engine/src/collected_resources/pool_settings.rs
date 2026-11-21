//! Original Settings rows reuse the same pure report as authenticated local originals.
use super::*;
use culprit_core::collected_resource_analysis::CollectedPoolSettingsAnalysis;
pub fn pool_settings(capture: &EvidenceCapture) -> Result<CollectedPoolSettingsAnalysis> {
    let inputs = CapturedState::sources_for(capture, SnapshotDataset::Settings, EvidenceFamily::SettingObservations)?;
    let mut rows = Vec::new();
    for input in inputs {
        rows.extend(input.settings()?);
    }
    let report = culprit_checks::pool_settings::explain(culprit_checks::pool_settings::PoolSettingsEvidence {
        source: capture.source(),
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })?;
    Ok(CollectedPoolSettingsAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
