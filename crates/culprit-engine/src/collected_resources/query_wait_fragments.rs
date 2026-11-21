//! Retain original QueryWaits family receipts, then delegate to the source-free pure fragment interpreter.
use super::*;
use culprit_core::query_wait_report::QueryWaitFragmentReport;

pub fn query_wait_fragments(capture: &EvidenceCapture) -> Result<QueryWaitFragmentReport> {
    culprit_checks::query_wait_fragments::explain(&original(capture)?)
}

pub(crate) fn original(capture: &EvidenceCapture) -> Result<CapturedQueryWaitRead> {
    references::validate_capture(capture)?;
    if capture.families() != [EvidenceFamily::QueryWaits] || capture.source().query_history().is_none() {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let read = references::captured_read(capture)?;
    let exports = capture
        .records()
        .iter()
        .map(|row| {
            let ordinal = row.row_ordinal().checked_sub(1).ok_or_else(invalid)?;
            if row.batch_id() != row.row().batch_id || row.native_payload_sha256() != row.row().payload_sha256()? {
                return Err(invalid());
            }
            Ok((row.row().clone(), ordinal))
        })
        .collect::<Result<Vec<_>>>()?;
    CapturedQueryWaitRead::from_exports(
        capture.source().clone(),
        read,
        capture.reference_coverage().clone(),
        (capture.collected_from().into(), capture.collected_until().into()),
        exports,
        capture.is_read_complete(),
    )
}
