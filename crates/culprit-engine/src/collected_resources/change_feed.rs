//! Authenticated original change-feed rows reuse the same pure explanation as local Outbox replay.
use super::*;
use culprit_core::collected_resource_analysis::CollectedChangeFeedAnalysis;
pub fn change_feed(capture: &EvidenceCapture) -> Result<CollectedChangeFeedAnalysis> {
    let mut rows = Vec::new();
    for source in CapturedState::sources(capture, SnapshotDataset::ChangeFeed)? {
        rows.extend(source.rows()?);
    }
    let report = culprit_checks::change_feed::explain(culprit_checks::change_feed::ChangeFeedEvidence {
        source: capture.source(),
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })?;
    Ok(CollectedChangeFeedAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
