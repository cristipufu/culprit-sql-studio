use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

#[tokio::test]
async fn native_object_definitions_preserve_identity_versions_payload_availability_and_source_visibility() {
    let cases = test_support::object_definition_evidence_cases();
    let first = &cases[0].1[0];
    let changed = &cases[4].1[0];
    assert_eq!(first.native_identity(), changed.native_identity());
    let (TelemetryRecord::ObjectDefinition(first), TelemetryRecord::ObjectDefinition(changed)) = (first, changed)
    else {
        panic!()
    };
    assert_ne!(
        first.version, changed.version,
        "newly unavailable module metadata changes the version without inventing another cache identity"
    );
    for (source, records) in cases {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.reply_families(FAMILY, 1);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..budget() }, None)
            .await
            .unwrap();
        test_support::assert_object_definition_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len());
        assert_eq!(capture.source().observation.snapshot(), source.observation.snapshot());
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn changed_native_handles_module_metadata_fingerprints_versions_or_keys_fail_original_object_proof() {
    let (source, records) = test_support::object_definition_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let schema = fixture.catalog.get(TelemetryStream::ObjectDefinitions).unwrap();
    let details_column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    for (column, pointer, value) in [
        ("Details", "/context/identity/sql_handle", json!("0x09090909")),
        ("Details", "/context/identity/plan_generation", json!("99")),
        ("Details", "/module_parameters/table_parameters/@Rows/1", json!("OtherRowsType")),
        ("Details", "/statement/native_sha256", json!(digest(b"another original native statement"))),
        ("DefinitionVersion", "", json!(digest(b"another definition version"))),
        ("FactKey", "", json!(digest(b"another native cache identity"))),
        ("NativeSource", "", json!("a-different-source")),
    ] {
        let mut rows = fixture.rows_for(EvidenceFamily::ObjectDefinitions);
        if column == "Details" {
            let mut details: Value = serde_json::from_str(rows[0][details_column].as_str().unwrap()).unwrap();
            *details.pointer_mut(pointer).unwrap() = value;
            if pointer == "/module_parameters/table_parameters/@Rows/1" {
                let TelemetryRecord::ObjectDefinition(original) = &records[0] else { panic!() };
                let mut fact = original.fact.clone();
                let ObjectDefinitionFact::MssqlCachedDefinition(definition) = &mut fact else { panic!() };
                definition
                    .module_parameters
                    .as_mut()
                    .unwrap()
                    .table_parameters
                    .insert("@Rows".into(), vec!["dbo".into(), "OtherRowsType".into()]);
                let updated = ObjectDefinitionRecord::new(original.source.clone(), fact).unwrap();
                // Fresh self-consistent metadata hashes cannot authorize a new native definition in the old receipt.
                let version = schema.columns().iter().position(|field| field.name == "DefinitionVersion").unwrap();
                rows[0][version] = json!(updated.version);
                let sha = schema.columns().iter().position(|field| field.name == "DetailSha256").unwrap();
                rows[0][sha] = json!(digest(&serde_json::to_vec(&details).unwrap()));
            }
            rows[0][details_column] = json!(serde_json::to_string(&details).unwrap());
        } else {
            let index = schema.columns().iter().position(|field| field.name == column).unwrap();
            rows[0][index] = value;
        }
        fixture.replies(vec![rows]);
        let error = fixture.reader().read_capture(&fixture.range, FAMILY, budget(), None).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{column}{pointer}"
        );
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn truncated_object_preview_requires_native_representation_without_rereading_payloads() {
    let (source, records) = test_support::object_definition_recovery_only_evidence();
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.reply_families(FAMILY, 250);
    let capture = fixture.reader().read_capture(&fixture.range, FAMILY, budget(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.gaps().len(), 1);
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert_eq!(capture.source(), &source);
    assert!(source.observation.snapshot().unwrap().sources[0].complete_scope);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
}

#[tokio::test]
async fn explicit_recovery_hydrates_large_native_plans_from_one_exact_batch_without_changing_source_coverage() {
    let (source, records) = test_support::object_definition_hydration_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let reads_before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.reply_families(FAMILY, 250);
    let preview = fixture.reader().read_capture(&fixture.range, FAMILY, budget(), None).await.unwrap();
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    assert_eq!(preview.gaps().len(), records.len());
    assert!(preview.gaps().iter().all(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), reads_before);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));

    let batch = &fixture.range.manifest().batches[0];
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.reply_families(FAMILY, 1);
    let hydrated = fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget {
                page_rows: 1,
                recovery_payloads: Some(RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes }),
                ..budget()
            },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    test_support::assert_object_definition_capture(&hydrated, &source, &records);
    assert_eq!(hydrated.metrics().pages as usize, records.len());
    assert_eq!(hydrated.metrics().recovery_objects, 1);
    assert_eq!(hydrated.metrics().recovery_bytes, batch.bytes);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst) - reads_before, 1);
    assert_eq!(hydrated.source(), preview.source());
    assert_eq!(hydrated.recovery_manifest_sha256(), fixture.range.manifest_digest());
    assert_eq!(hydrated.capture_manifest_sha256(), fixture.range.manifest().capture_manifest_sha256);
}

#[tokio::test]
async fn recovery_hydration_cannot_repair_a_changed_native_definition_receipt() {
    let (source, records) = test_support::object_definition_hydration_evidence();
    let fixture = Fixture::new(source, records).await;
    let reads_before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let schema = fixture.catalog.get(TelemetryStream::ObjectDefinitions).unwrap();
    let version = schema.columns().iter().position(|column| column.name == "DefinitionVersion").unwrap();
    let mut rows = fixture.rows_for(EvidenceFamily::ObjectDefinitions);
    rows[0][version] = json!(digest(b"unrelated native definition version"));
    fixture.replies(vec![rows]);
    let error = fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..budget() },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), reads_before);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::None));
}

#[tokio::test]
async fn missing_warehouse_definition_rows_do_not_become_native_empty_or_complete_read() {
    let (source, records) = test_support::object_definition_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records).await;
    let mut rows = fixture.rows_for(EvidenceFamily::ObjectDefinitions);
    rows.remove(1);
    fixture.replies(vec![rows]);
    let capture = fixture.reader().read_capture(&fixture.range, FAMILY, budget(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert_eq!(capture.records().len(), 2);
    assert_eq!(capture.gaps().len(), 1);
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
    assert_eq!(capture.source(), &source);
    assert_eq!(source.observation.snapshot().unwrap().records, 3);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
}
