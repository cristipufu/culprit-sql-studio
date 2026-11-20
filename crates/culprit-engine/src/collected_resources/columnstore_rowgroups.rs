//! Exact original rows enter the same pure interpretation; no native reader or extra recovery work.
use super::*;
use culprit_core::collected_resource_analysis::CollectedColumnstoreRowgroupAnalysis;

pub fn columnstore_rowgroups(capture: &EvidenceCapture) -> Result<CollectedColumnstoreRowgroupAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::ColumnstoreRowgroups)?;
    let rows = input.rows()?;
    let report = culprit_checks::columnstore_rowgroups::explain(
        culprit_checks::columnstore_rowgroups::ColumnstoreRowgroupEvidence {
            source: capture.source(),
            observations: &rows,
            read_complete: capture.is_read_complete(),
        },
    )?;
    Ok(CollectedColumnstoreRowgroupAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
