use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn exact_native_current_work_and_immutable_focus_round_trip_without_opening_payloads() {
    let cases = test_support::current_request_work_evidence_cases();
    assert_eq!(cases.len(), 13);
    for (source, records) in cases {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let public = serde_json::to_string(&fixture.publication).unwrap();
        for private in ["private-current-work-client", "private-current-work-login", "private_current_column_"] {
            assert!(!public.contains(private));
        }
        fixture.replies(vec![fixture.rows.clone()]);
        let capture = read(&fixture, false).await.unwrap();
        assert_eq!(capture.source(), &source);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
        let long = records.iter().any(|row| {
            row.definition_payloads()
                .any(|definition| matches!(definition.payload,DefinitionPayload::Available{text,..}if text.len()>1024))
        });
        if long {
            assert!(!capture.is_read_complete());
            assert!(capture.records().is_empty());
            assert!(matches!(
                capture.gaps()[0].reason,
                EvidenceGapReason::DetailInRecovery | EvidenceGapReason::NativeRepresentationRequired
            ));
        } else {
            test_support::assert_current_request_work_capture(&capture, &source, &records);
        }
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn full_selected_sql_hydrates_once_but_a_missing_warehouse_point_is_not_resurrected() {
    let (source, records) = test_support::current_request_work_evidence_cases().pop().unwrap();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![fixture.rows.clone()]);
    let capture = read(&fixture, true).await.unwrap();
    test_support::assert_current_request_work_capture(&capture, &source, &records);
    assert_eq!(capture.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 1);
    assert_eq!(capture.capture_manifest_sha256(), fixture.range.manifest().capture_manifest_sha256);
    fixture.replies(vec![vec![]]);
    let absent = read(&fixture, true).await.unwrap();
    assert!(!absent.is_read_complete());
    assert!(absent.records().is_empty());
    assert_eq!(absent.gaps()[0].reason, EvidenceGapReason::MissingRows);
    assert_eq!(absent.source(), &source);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 1);
}

#[tokio::test]
async fn actor_event_stage_parent_scope_and_exact_counter_tampering_fail_before_payload_recovery() {
    let (source, records) = test_support::current_request_work_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    for (pointer, value) in [
        ("/actor/session/generation", json!("108")),
        ("/actor/request/id", json!("99")),
        ("/native/thread_id", json!("108")),
        ("/native/statement_event_id", json!("99")),
        ("/native/stage/event/nesting_event_id", json!("99")),
        ("/native/stage/event/nesting_event_type", json!("STAGE")),
        ("/native/rows_examined", json!("9007199254740992")),
        ("/native/created_tmp_disk_tables", Value::Null),
        ("/database", json!("another_database")),
        ("/observed_at", json!("2020-01-01T11:59:00Z")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut body: Value = serde_json::from_str(rows[0][details].as_str().unwrap()).unwrap();
        *body.pointer_mut(pointer).expect("declared native point field") = value;
        rows[0][details] = json!(serde_json::to_string(&body).unwrap());
        fixture.replies(vec![rows]);
        let error = read(&fixture, true).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
    for field in ["SourceId", "LogicalSourceId"] {
        let column = schema.columns().iter().position(|column| column.name == field).unwrap();
        let mut rows = fixture.rows.clone();
        rows[0][column] = json!(digest(b"wrong-current-work-source-or-native-epoch"));
        fixture.replies(vec![rows]);
        assert_eq!(
            read(&fixture, true).await.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity))
        );
    }
    let inventory = schema.columns().iter().position(|column| column.name == "DefinitionPayloads").unwrap();
    let mut rows = fixture.rows.clone();
    let mut payloads: Value = serde_json::from_str(rows[0][inventory].as_str().unwrap()).unwrap();
    assert_eq!(payloads[0]["pointer"], "/record/data/fact/data/statement");
    payloads[0]["sha256"] = json!(digest(b"different-selected-statement"));
    rows[0][inventory] = json!(serde_json::to_string(&payloads).unwrap());
    fixture.replies(vec![rows]);
    assert_eq!(
        read(&fixture, true).await.unwrap_err().detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity))
    );
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
}

#[tokio::test]
async fn changed_public_focus_membership_role_or_count_cannot_replace_the_authenticated_native_header() {
    for (pointer, value) in [
        ("/observation/data/focus_key", json!(digest(b"another-native-request-parent"))),
        ("/observation/data/membership", json!("unavailable")),
        ("/observation/data/database_writable", json!(false)),
        ("/observation/data/snapshot/records", json!(0)),
    ] {
        let (source, records) = test_support::current_request_work_evidence_cases().remove(0);
        let mut fixture = Fixture::new(source, records).await;
        let column = fixture
            .catalog
            .publication()
            .columns()
            .iter()
            .position(|column| column.name == "SourceDescription")
            .unwrap();
        let mut public: Value = serde_json::from_str(fixture.publication[column].as_str().unwrap()).unwrap();
        *public.pointer_mut(pointer).unwrap() = value;
        fixture.publication[column] = json!(serde_json::to_string(&public).unwrap());
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        assert_eq!(
            read(&fixture, false).await.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{pointer}"
        );
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn corrupt_truncated_or_late_failing_native_payloads_never_release_a_repaired_point() {
    let (source, records) = test_support::current_request_work_evidence_cases().pop().unwrap();
    let fixture = Fixture::new(source, records).await;
    for fault in [Fault::Corrupt, Fault::Truncate, Fault::LateError] {
        *fixture.recovery.fault.lock().unwrap() = fault;
        fixture.replies(vec![fixture.rows.clone()]);
        assert!(read(&fixture, true).await.is_err());
    }
}

async fn read(fixture: &Fixture, recovery: bool) -> culprit_core::Result<EvidenceCapture> {
    fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget {
                recovery_payloads: recovery.then(RecoveryPayloadBudget::default),
                ..Default::default()
            },
            recovery.then_some(fixture.recovery.as_ref() as &dyn culprit_collector::delivery::recovery::RecoveryStore),
        )
        .await
}
