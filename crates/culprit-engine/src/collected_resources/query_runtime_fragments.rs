use super::*;
use culprit_core::query_runtime_report::QueryRuntimeFragmentReport;
pub fn query_runtime_fragments(capture: &EvidenceCapture) -> Result<QueryRuntimeFragmentReport> {
    culprit_checks::query_runtime_fragments::explain(&super::query_history_fragments::original(
        capture,
        TelemetryStream::QueryRuntime,
    )?)
}
