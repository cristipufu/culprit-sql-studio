use super::*;
use std::sync::atomic::Ordering;
use test_support::{MemoryFeedbackCase, memory_feedback_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

#[tokio::test]
async fn retained_feedback_corpus_hydrates_exact_native_records_once_without_reinterpreting_visibility() {
    for case in MemoryFeedbackCase::ALL {
        let (source, records) = memory_feedback_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.reply_families(FAMILY, 1);
        let preview = read(&fixture, false).await.unwrap();
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens, "{case:?}");
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        assert_eq!(preview.source(), &source);
        if matches!(case, MemoryFeedbackCase::SourceUnavailable | MemoryFeedbackCase::PayloadUnavailable) {
            test_support::assert_object_definition_capture(&preview, &source, &records);
            assert_eq!(preview.metrics().recovery_objects, 0);
            continue;
        }
        assert!(!preview.is_read_complete(), "{case:?}");
        assert!(preview.records().is_empty());
        assert_eq!(preview.gaps().len(), 1);
        assert_eq!(preview.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        fixture.reply_families(FAMILY, 1);
        let capture = read(&fixture, true).await.unwrap();
        test_support::assert_object_definition_capture(&capture, &source, &records);
        let batch = &fixture.range.manifest().batches[0];
        assert_eq!(capture.metrics().recovery_objects, 1);
        assert_eq!(capture.metrics().recovery_bytes, batch.bytes);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 1);
        assert_eq!(capture.capture_manifest_sha256(), fixture.range.manifest().capture_manifest_sha256);
        assert_eq!(capture.recovery_manifest_sha256(), fixture.range.manifest_digest());
    }
}

#[tokio::test]
async fn feedback_recovery_cannot_replace_a_missing_warehouse_row_or_native_source_state() {
    let (source, records) = memory_feedback_evidence(MemoryFeedbackCase::DisabledUnstable);
    let fixture = Fixture::new(source.clone(), records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![vec![]]);
    let capture = read(&fixture, true).await.unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
    assert_eq!(capture.source(), &source);
    assert!(source.observation.snapshot().unwrap().sources[0].complete_scope);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
}

#[tokio::test]
async fn changed_retained_feedback_identity_version_or_native_payload_digest_fails_before_recovery_open() {
    let (source, records) = memory_feedback_evidence(MemoryFeedbackCase::DisabledUnstable);
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::ObjectDefinitions).unwrap();
    let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    for (column, pointer, value) in [
        ("Details", "/context/identity/sql_handle", json!("0x09090909")),
        ("Details", "/batch_plan/native_sha256", json!(digest(b"another native feedback body"))),
        ("DefinitionVersion", "", json!(digest(b"another retained definition"))),
        ("NativeSource", "", json!("another-native-cache-source")),
    ] {
        let mut rows = fixture.rows_for(EvidenceFamily::ObjectDefinitions);
        if column == "Details" {
            let mut body: Value = serde_json::from_str(rows[0][details].as_str().unwrap()).unwrap();
            *body.pointer_mut(pointer).unwrap() = value;
            rows[0][details] = json!(serde_json::to_string(&body).unwrap());
        } else {
            let index = schema.columns().iter().position(|field| field.name == column).unwrap();
            rows[0][index] = value;
        }
        fixture.replies(vec![rows]);
        let error = read(&fixture, true).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{column}{pointer}"
        );
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn feedback_native_tail_must_verify_through_eof_before_any_record_is_returned() {
    let (source, records) = memory_feedback_evidence(MemoryFeedbackCase::DisabledUnstable);
    let fixture = Fixture::new(source, records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    for fault in [Fault::Corrupt, Fault::Truncate, Fault::LateError] {
        *fixture.recovery.fault.lock().unwrap() = fault;
        fixture.reply_families(FAMILY, 1);
        assert!(read(&fixture, true).await.is_err());
    }
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 3);
}

async fn read(fixture: &Fixture, recovery: bool) -> culprit_core::Result<EvidenceCapture> {
    fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget {
                page_rows: 1,
                recovery_payloads: recovery.then(RecoveryPayloadBudget::default),
                ..budget()
            },
            recovery.then_some(fixture.recovery.as_ref() as &dyn culprit_collector::delivery::recovery::RecoveryStore),
        )
        .await
}
