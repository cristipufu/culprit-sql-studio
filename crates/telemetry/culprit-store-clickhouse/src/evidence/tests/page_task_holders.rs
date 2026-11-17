use super::*;
use std::sync::atomic::Ordering;
use test_support::{PageTaskHolderCase, page_task_holder_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn all_native_holder_points_keep_exact_receipts_unknowns_and_clocks_through_preview_and_recovery() {
    for case in PageTaskHolderCase::ALL {
        let (source, records) = page_task_holder_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let rows = native_rows(&fixture);
        let public = serde_json::to_string(&rows).unwrap();
        for private in
            ["private-writer-machine-canary", "private-writer-instance-canary", "original-writer-native-process"]
        {
            assert!(!public.contains(private), "native source authority is only in the authenticated header: {case:?}");
        }
        fixture.replies(vec![rows.clone()]);
        let preview = read(&fixture, None).await.unwrap();
        assert_eq!(preview.source(), &source);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
        let needs_body = preview.records().is_empty();
        if case == PageTaskHolderCase::LongSql {
            assert!(needs_body, "a SQL preview cannot become an exact native holder verdict");
        }
        if needs_body {
            assert!(!preview.is_read_complete());
            assert!(matches!(
                preview.gaps()[0].reason,
                EvidenceGapReason::DetailInRecovery | EvidenceGapReason::NativeRepresentationRequired
            ));
        } else {
            test_support::assert_operational_capture(&preview, &source, &records);
        }
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        fixture.replies(vec![rows]);
        let capture = read(&fixture, Some(RecoveryPayloadBudget::default())).await.unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(capture.is_read_complete(), "{case:?}: {:?}", capture.gaps());
        assert_eq!(capture.metrics().recovery_objects, u32::from(needs_body));
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, usize::from(needs_body));
        assert_eq!(capture.capture_manifest_sha256(), fixture.range.manifest().capture_manifest_sha256);
        assert_eq!(capture.recovery_manifest_sha256(), fixture.range.manifest_digest());
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::PageTaskHolders(point), ..
        }) = &capture.records()[0].row().record
        else {
            panic!("typed holder body")
        };
        assert_ne!(point.before.observed_at, point.after.observed_at);
        let SourceObservation::PageTaskHolders { focus, .. } = &capture.source().observation else {
            panic!("original focus")
        };
        assert_eq!(focus.task.wait_duration_ms.as_deref(), Some("9007199254740993"));
        assert_eq!(focus.participant.original.wait.as_ref().unwrap().native_type.as_deref(), Some("CXCONSUMER"));
        assert_eq!(focus.task.wait_type, "PAGELATCH_EX");
        assert_eq!(capture.records()[0].native_payload_sha256(), capture.records()[0].row().payload_sha256().unwrap());
    }
}

#[tokio::test]
async fn missing_stored_body_is_an_independent_gap_and_recovery_never_fills_absent_membership() {
    for case in [PageTaskHolderCase::PartialWarehouseBody, PageTaskHolderCase::PermissionUnavailable] {
        let (source, records) = page_task_holder_evidence(case);
        let fixture = Fixture::new(source.clone(), records).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        fixture.replies(vec![vec![]]);
        let capture = read(&fixture, Some(RecoveryPayloadBudget::default())).await.unwrap();
        assert!(!capture.is_read_complete());
        assert!(capture.records().is_empty());
        assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
        assert_eq!(capture.source(), &source);
        assert_eq!(source.observation.snapshot().unwrap().records, 1);
        assert_eq!(source.observation.snapshot().unwrap().sources[0].state, culprit_core::SourceState::Partial);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn altered_native_task_request_pointer_clock_payload_or_source_proof_fails_before_recovery() {
    let (source, records) = page_task_holder_evidence(PageTaskHolderCase::QualifiedBoth);
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let details = schema.columns().iter().position(|field| field.name == "Details").unwrap();
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    for (pointer, value) in [
        ("/before/holder/request/key/session/generation", json!("recycled-holder-session")),
        ("/after/holder/request/key/request/generation", json!("recycled-holder-request")),
        ("/after_task/blocking_task_address", json!("0000000000000099")),
        ("/before/holder/task/execution_context_id", json!(99)),
        ("/after/database_writable", json!(false)),
        ("/after/observed_at", json!("2020-01-01T11:59:00Z")),
        ("/focus_key", json!(digest(b"different original activity and page receipts"))),
        ("/page/target/file/qualifiers/create_lsn", json!("foreign-native-file-incarnation")),
    ] {
        let mut rows = native_rows(&fixture);
        let mut body: Value = serde_json::from_str(rows[0][details].as_str().unwrap()).unwrap();
        *body.pointer_mut(pointer).expect("declared holder point field") = value;
        rows[0][details] = json!(serde_json::to_string(&body).unwrap());
        fixture.replies(vec![rows]);
        assert_integrity(read(&fixture, Some(RecoveryPayloadBudget::default())).await.unwrap_err(), pointer);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
    for field in ["SourceId", "LogicalSourceId", "RecordId"] {
        let column = schema.columns().iter().position(|column| column.name == field).unwrap();
        let mut rows = native_rows(&fixture);
        rows[0][column] = json!(digest(b"different original scope, generation or row receipt"));
        fixture.replies(vec![rows]);
        assert_integrity(read(&fixture, Some(RecoveryPayloadBudget::default())).await.unwrap_err(), field);
    }
    let inventory = schema.columns().iter().position(|field| field.name == "DefinitionPayloads").unwrap();
    let mut rows = native_rows(&fixture);
    let mut payloads: Value = serde_json::from_str(rows[0][inventory].as_str().unwrap()).unwrap();
    let selected = payloads
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|payload| payload["pointer"] == "/record/data/fact/data/before/holder/request/statement")
        .unwrap();
    selected["sha256"] = json!(digest(b"another exact holder SQL payload"));
    rows[0][inventory] = json!(serde_json::to_string(&payloads).unwrap());
    fixture.replies(vec![rows]);
    assert_integrity(read(&fixture, Some(RecoveryPayloadBudget::default())).await.unwrap_err(), "payload inventory");
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
}

#[tokio::test]
async fn changed_public_original_focus_role_or_native_count_cannot_replace_the_authenticated_header() {
    for (pointer, value) in [
        ("/observation/data/focus_key", json!(digest(b"another admitted holder question"))),
        ("/observation/data/database_writable", Value::Null),
        ("/observation/data/snapshot/records", json!(0)),
    ] {
        let (source, records) = page_task_holder_evidence(PageTaskHolderCase::QualifiedBoth);
        let mut fixture = Fixture::new(source, records).await;
        let column =
            fixture.catalog.publication().columns().iter().position(|field| field.name == "SourceDescription").unwrap();
        let mut description: Value = serde_json::from_str(fixture.publication[column].as_str().unwrap()).unwrap();
        *description.pointer_mut(pointer).expect("declared public holder header field") = value;
        fixture.publication[column] = json!(serde_json::to_string(&description).unwrap());
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![native_rows(&fixture)]);
        assert_integrity(read(&fixture, None).await.unwrap_err(), pointer);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn insufficient_recovery_budget_and_corrupt_truncated_or_late_failing_bodies_never_release_a_holder_prefix() {
    let (source, records) = page_task_holder_evidence(PageTaskHolderCase::LongSql);
    let fixture = Fixture::new(source, records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![native_rows(&fixture)]);
    let bytes = fixture.range.manifest().batches[0].bytes;
    let error = read(&fixture, Some(RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes - 1 })).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    for fault in [Fault::Corrupt, Fault::Truncate, Fault::LateError] {
        *fixture.recovery.fault.lock().unwrap() = fault;
        fixture.replies(vec![native_rows(&fixture)]);
        assert!(read(&fixture, Some(RecoveryPayloadBudget::default())).await.is_err());
    }
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 3);
}

fn assert_integrity(error: culprit_core::Error, field: &str) {
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{field}");
}

fn native_rows(fixture: &Fixture) -> Vec<Vec<Value>> {
    fixture.rows.clone()
}
async fn read(fixture: &Fixture, recovery: Option<RecoveryPayloadBudget>) -> culprit_core::Result<EvidenceCapture> {
    let enabled = recovery.is_some();
    fixture
        .reader
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: recovery, ..Default::default() },
            enabled.then_some(fixture.recovery.as_ref() as &dyn culprit_collector::delivery::recovery::RecoveryStore),
        )
        .await
}
