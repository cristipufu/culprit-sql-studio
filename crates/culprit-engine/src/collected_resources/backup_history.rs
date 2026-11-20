//! Original database backup metadata enters one canonical interpretation without consulting recovery media.
use super::*;
use culprit_core::collected_resource_analysis::CollectedBackupHistoryAnalysis;

pub fn backup_history(capture: &EvidenceCapture) -> Result<CollectedBackupHistoryAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::BackupHistory)?;
    let rows = input.rows()?;
    let report = culprit_checks::backup_history::explain(culprit_checks::backup_history::BackupHistoryEvidence {
        source: capture.source(),
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })?;
    Ok(CollectedBackupHistoryAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
