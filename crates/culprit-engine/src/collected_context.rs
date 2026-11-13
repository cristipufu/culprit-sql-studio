//! Both archived-analysis views retain the same authenticated capture and native-row identity.
use culprit_collector::analytical::evidence::EvidenceCapture;
use culprit_core::collected_analysis::{CollectedAnalysisRecord, CollectedEvidenceContext};

pub(crate) fn from_capture(capture: &EvidenceCapture) -> CollectedEvidenceContext {
    CollectedEvidenceContext {
        range_id: capture.range_id().into(),
        manifest_sha256: capture.manifest_sha256().into(),
        source: capture.source().clone(),
        capture: capture.capture().clone(),
        collected_from: capture.collected_from().into(),
        collected_until: capture.collected_until().into(),
        selected_streams: capture.families().iter().map(|family| family.stream()).collect(),
        read_gaps: capture.gaps().to_vec(),
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
