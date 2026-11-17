//! Both archived-analysis views retain the same authenticated capture and native-row identity.
use culprit_collector::analytical::evidence::{EvidenceCapture, EvidenceReadMetrics};
use culprit_core::collected_analysis::{CollectedAnalysisRecord, CollectedEvidenceContext, CollectedEvidenceMetrics};

pub(crate) fn from_capture(capture: &EvidenceCapture) -> CollectedEvidenceContext {
    CollectedEvidenceContext {
        range_id: capture.range_id().into(),
        recovery_manifest_sha256: capture.recovery_manifest_sha256().into(),
        capture_manifest_sha256: capture.capture_manifest_sha256().into(),
        source: capture.source().clone(),
        capture: capture.capture().clone(),
        collected_from: capture.collected_from().into(),
        collected_until: capture.collected_until().into(),
        selected_streams: capture.families().iter().map(|family| family.stream()).collect(),
        read_gaps: capture.gaps().to_vec(),
        metrics: read_metrics(capture.metrics()),
        records: capture
            .records()
            .iter()
            .map(|record| CollectedAnalysisRecord {
                batch_id: record.batch_id().into(),
                row_ordinal: record.row_ordinal(),
                native_payload_sha256: record.native_payload_sha256().into(),
                row: record.row().clone(),
            })
            .collect(),
    }
}

pub(crate) fn read_metrics(metrics: &EvidenceReadMetrics) -> CollectedEvidenceMetrics {
    CollectedEvidenceMetrics {
        selected_rows: metrics.selected_rows,
        queries: metrics.queries,
        pages: metrics.pages,
        returned_rows: metrics.returned_rows,
        response_bytes: metrics.response_bytes,
        proof_bytes: metrics.proof_bytes,
        recovery_objects: metrics.recovery_objects,
        recovery_bytes: metrics.recovery_bytes,
    }
}
