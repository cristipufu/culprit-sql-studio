use super::*;
use std::sync::atomic::Ordering;
use test_support::{ParameterEvidenceCase as Case, parameter_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

#[tokio::test]
async fn original_scalar_corpus_replays_exact_source_facts_only_after_authenticated_native_hydration() {
    for case in Case::ALL {
        let (source, records) = parameter_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.reply_families(FAMILY, 1);
        let preview = read(&fixture, false).await.unwrap();
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens, "{case:?}");
        assert_eq!(preview.source(), &source);
        if case == Case::SourceUnavailable {
            test_support::assert_object_definition_capture(&preview, &source, &records);
            assert!(preview.records().is_empty());
            continue;
        }
        if case == Case::PayloadUnavailable {
            // Native unavailability is the complete recorded fact. Redaction removes the SQL comment, so
            // this row has no truncated available payload and needs no recovery object to reconstruct it.
            test_support::assert_object_definition_capture(&preview, &source, &records);
            fixture.reply_families(FAMILY, 1);
            let capture = read(&fixture, true).await.unwrap();
            test_support::assert_object_definition_capture(&capture, &source, &records);
            assert_eq!(capture.metrics().recovery_objects, 0);
            assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
            continue;
        }
        assert!(!preview.is_read_complete(), "{case:?}");
        assert!(preview.records().is_empty());
        assert_eq!(preview.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        fixture.reply_families(FAMILY, 1);
        let capture = read(&fixture, true).await.unwrap();
        test_support::assert_object_definition_capture(&capture, &source, &records);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 1);
        assert_eq!(capture.metrics().recovery_objects, 1);
        assert_eq!(capture.capture_manifest_sha256(), fixture.range.manifest().capture_manifest_sha256);
        assert_eq!(capture.recovery_manifest_sha256(), fixture.range.manifest_digest());
        let encoded = serde_json::to_string(&capture.records()).unwrap();
        for secret in ["private-original-compiled-canary", "private-original-runtime-canary"] {
            assert!(!encoded.contains(secret), "{case:?}: {secret}");
        }
    }
}

#[tokio::test]
async fn scalar_proof_receipts_reject_replaced_node_comparison_source_native_body_or_version_before_open() {
    let (source, records) = parameter_evidence(Case::AccessMismatch);
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::ObjectDefinitions).unwrap();
    let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    for (column, pointer, value) in [
        ("Details", "/parameter_context_proof/context/uses/0/node_id", json!(8)),
        ("Details", "/parameter_context_proof/context/parameters/0/comparison", json!("same-representation")),
        ("Details", "/parameter_context_proof/source_scope", json!(digest(b"other-original-scope"))),
        ("Details", "/context/identity/plan_generation", json!("99")),
        ("Details", "/batch_plan/native_sha256", json!(digest(b"other-original-native-body"))),
        ("DefinitionVersion", "", json!(digest(b"other-definition"))),
        ("NativeSource", "", json!("other-native-source")),
    ] {
        let mut rows = fixture.rows_for(EvidenceFamily::ObjectDefinitions);
        if column == "Details" {
            let mut body: Value = serde_json::from_str(rows[0][details].as_str().unwrap()).unwrap();
            *body.pointer_mut(pointer).unwrap() = value;
            rows[0][details] = json!(serde_json::to_string(&body).unwrap());
        } else {
            let at = schema.columns().iter().position(|field| field.name == column).unwrap();
            rows[0][at] = value;
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
async fn missing_membership_and_unverified_native_tail_never_return_a_scalar_proof() {
    let (source, records) = parameter_evidence(Case::AccessMismatch);
    let fixture = Fixture::new(source.clone(), records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![vec![]]);
    let missing = read(&fixture, true).await.unwrap();
    assert_eq!(missing.source(), &source);
    assert!(missing.records().is_empty());
    assert_eq!(missing.gaps()[0].reason, EvidenceGapReason::MissingRows);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
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
