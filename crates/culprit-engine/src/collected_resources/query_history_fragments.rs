//! One immutable QueryHistory family conversion; neither a source reader nor a native decoder.
use super::*;
pub(super) fn original(capture: &EvidenceCapture, stream: TelemetryStream) -> Result<CapturedQueryHistoryRead> {
    references::validate_capture(capture)?;
    if !matches!(stream, TelemetryStream::QueryRuntime | TelemetryStream::QueryWaits)
        || capture.families().len() != 1
        || capture.families()[0].stream() != stream
        || capture.source().query_history().is_none()
    {
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
    CapturedQueryHistoryRead::from_exports(
        capture.source().clone(),
        read,
        capture.reference_coverage().clone(),
        (capture.collected_from().into(), capture.collected_until().into()),
        exports,
        capture.is_read_complete(),
    )
}
