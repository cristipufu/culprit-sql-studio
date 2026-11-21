//! The selected authenticated diagnostic stream is read once; referenced application evidence is not reopened.
use super::*;
use culprit_core::collected_resource_analysis::CollectedIncidentHistoryAnalysis;

pub fn incident_history(capture: &EvidenceCapture) -> Result<CollectedIncidentHistoryAnalysis> {
    references::validate_capture(capture)?;
    if capture.families() != [EvidenceFamily::OperationalSamples] {
        return Err(invalid());
    }
    let original = DiagnosticCaptureRef {
        capture: capture.capture().clone(),
        manifest_sha256: capture.capture_manifest_sha256().into(),
        source_scope_id: capture.source().captured_range()?.scope_id,
    };
    let rows = capture
        .records()
        .iter()
        .map(|row| {
            let reference = references::from_record(capture, row)?;
            Ok((row.row(), reference.row_ordinal))
        })
        .collect::<Result<Vec<_>>>()?;
    let report =
        crate::incident_history::from_original(capture.source(), &original, &rows, capture.is_read_complete())?;
    Ok(CollectedIncidentHistoryAnalysis {
        context: crate::collected_context::from_capture(capture),
        report: Box::new(report),
    })
}
