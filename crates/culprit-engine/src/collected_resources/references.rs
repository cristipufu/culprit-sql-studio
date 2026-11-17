//! Original archive receipts share one validation path across native point and Activity explanations.
use super::*;
use culprit_collector::analytical::evidence::VerifiedEvidenceRecord;

pub(super) fn validate_capture(capture: &EvidenceCapture) -> Result<()> {
    if !valid_digest(capture.range_id())
        || !valid_digest(capture.recovery_manifest_sha256())
        || !valid_digest(capture.capture_manifest_sha256())
        || ObservationBounds::new(capture.collected_from(), capture.collected_until()).is_none()
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn from_record(capture: &EvidenceCapture, record: &VerifiedEvidenceRecord) -> Result<DiagnosticEvidenceRef> {
    let row = record.row();
    let ordinal = record.row_ordinal().checked_sub(1).ok_or_else(invalid)?;
    if row.capture != *capture.capture()
        || row.batch_id != record.batch_id()
        || row.payload_sha256()? != record.native_payload_sha256()
    {
        return Err(invalid());
    }
    DiagnosticEvidenceRef::from_export(capture.source(), capture.capture_manifest_sha256(), row, ordinal)
}
