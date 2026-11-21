//! The native wait family delegates to the same authenticated history converter as runtime fragments.
use super::*;
use culprit_core::query_wait_report::QueryWaitFragmentReport;
pub fn query_wait_fragments(capture: &EvidenceCapture) -> Result<QueryWaitFragmentReport> {
    culprit_checks::query_wait_fragments::explain(&super::query_history_fragments::original(
        capture,
        TelemetryStream::QueryWaits,
    )?)
}
