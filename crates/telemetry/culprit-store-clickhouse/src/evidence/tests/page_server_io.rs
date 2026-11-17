use super::*;
use std::sync::atomic::Ordering;
use test_support::{PageServerIoCase as Case, page_server_io_evidence};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

#[tokio::test]
async fn page_server_native_xml_replays_original_scope_identity_and_unknowns_after_exact_hydration() {
    for case in Case::ALL {
        let (source, records) = page_server_io_evidence(case);
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::ObjectDefinitions).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let preview = read(&fixture, false).await.unwrap();
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens, "{case:?}");
        assert_eq!(preview.source(), &source);
        if matches!(case, Case::SourceUnavailable | Case::PayloadUnavailable) {
            test_support::assert_object_definition_capture(&preview, &source, &records);
            assert_eq!(preview.metrics().recovery_objects, 0);
            continue;
        }
        assert!(!preview.is_read_complete(), "{case:?}");
        assert!(preview.records().is_empty());
        assert_eq!(preview.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        fixture.replies(vec![fixture.rows.clone()]);
        let capture = read(&fixture, true).await.unwrap();
        test_support::assert_object_definition_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().recovery_objects, 1);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 1);
        assert_eq!(capture.capture_manifest_sha256(), fixture.range.manifest().capture_manifest_sha256);
        assert_eq!(capture.recovery_manifest_sha256(), fixture.range.manifest_digest());
        let original = retained(&records[0]);
        let restored = retained(&capture.records()[0].row().record);
        assert_eq!(restored.context.identity, original.context.identity);
        assert_eq!(restored.context.database_id, original.context.database_id);
        assert_eq!(restored.context.query_hash, original.context.query_hash);
        assert_eq!(restored.batch_plan.native_sha256, original.batch_plan.native_sha256);
        assert_eq!(restored.statement_executions.as_deref(), Some("9007199254740993"));
        let encoded = serde_json::to_string(&capture.records()).unwrap();
        assert!(!encoded.contains("private-page-sql-canary"));
        if matches!(case, Case::Complete | Case::PartialSource | Case::UnqualifiedOwner) {
            let DefinitionPayload::Available { text, .. } = &restored.batch_plan.payload else { panic!() };
            assert!(text.contains("ActualPageServerReads=\"9007199254740993\""));
            assert!(text.contains("ActualLobPageServerReadAheads=\"0\""));
        }
    }
}

#[tokio::test]
async fn page_server_statement_scope_and_native_body_tampering_fail_before_recovery_open() {
    let (source, records) = page_server_io_evidence(Case::Complete);
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::ObjectDefinitions).await;
    let schema = fixture.catalog.get(TelemetryStream::ObjectDefinitions).unwrap();
    let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    for (column, pointer, value) in [
        ("Details", "/context/identity/plan_generation", json!("99")),
        ("Details", "/context/identity/sql_handle", json!("0xAABBCCDD")),
        ("Details", "/context/database_id", json!(99)),
        ("Details", "/context/query_hash", json!("0xFEDCBA9876543210")),
        ("Details", "/batch_plan/native_sha256", json!(digest(b"another original page-server XML"))),
        ("DefinitionVersion", "", json!(digest(b"another retained definition"))),
        ("NativeSource", "", json!("another-native-cache-source")),
    ] {
        let mut rows = fixture.rows_for(TelemetryStream::ObjectDefinitions);
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
async fn missing_warehouse_membership_or_unverified_tail_never_supplies_remote_page_facts() {
    let (source, records) = page_server_io_evidence(Case::Complete);
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::ObjectDefinitions).await;
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
        fixture.replies(vec![fixture.rows.clone()]);
        assert!(read(&fixture, true).await.is_err());
    }
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - opens, 3);
}

fn retained(record: &TelemetryRecord) -> &MssqlRetainedPlanDefinition {
    let TelemetryRecord::ObjectDefinition(record) = record else { panic!() };
    let ObjectDefinitionFact::MssqlRetainedPlan(native) = &record.fact else { panic!() };
    native
}

async fn read(fixture: &Fixture, recovery: bool) -> culprit_core::Result<EvidenceCapture> {
    fixture
        .reader
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget {
                page_rows: 1,
                recovery_payloads: recovery.then(RecoveryPayloadBudget::default),
                ..Default::default()
            },
            recovery.then_some(fixture.recovery.as_ref() as &dyn culprit_collector::delivery::recovery::RecoveryStore),
        )
        .await
}
