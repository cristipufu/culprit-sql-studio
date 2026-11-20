//! Authenticated original rows enter the same CDC interpretation as the local Outbox action.
use super::*;
use culprit_core::collected_resource_analysis::CollectedCdcAnalysis;

pub fn cdc(capture: &EvidenceCapture) -> Result<CollectedCdcAnalysis> {
    let mut rows = Vec::new();
    for source in CapturedState::sources(capture, SnapshotDataset::Cdc)? {
        rows.extend(source.rows()?);
    }
    let report = culprit_checks::cdc::explain(culprit_checks::cdc::CdcEvidence {
        source: capture.source(),
        observations: &rows,
        read_complete: capture.is_read_complete(),
    })?;
    Ok(CollectedCdcAnalysis { context: crate::collected_context::from_capture(capture), report: Box::new(report) })
}
