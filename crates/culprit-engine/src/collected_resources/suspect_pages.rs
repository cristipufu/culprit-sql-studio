use super::*;
use culprit_core::collected_resource_analysis::CollectedSuspectPageHistoryAnalysis;
pub fn suspect_pages(capture: &EvidenceCapture) -> Result<CollectedSuspectPageHistoryAnalysis> {
    let input = CapturedState::sources(capture, SnapshotDataset::SuspectPages)?
        .into_iter()
        .find(|input| input.coverage.source == culprit_core::source_id::SUSPECT_PAGES)
        .ok_or_else(invalid)?;
    let read = SuspectPageHistoryRead::from_source(capture.source(), &input.rows()?, capture.is_read_complete())
        .map_err(|_| invalid())?;
    let report = culprit_checks::suspect_pages::explain(&read).map_err(|_| invalid())?;
    Ok(CollectedSuspectPageHistoryAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
