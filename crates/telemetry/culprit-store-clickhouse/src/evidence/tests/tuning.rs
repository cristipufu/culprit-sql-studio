use super::*;
use std::sync::atomic::Ordering;
const FAMILIES: &[EvidenceFamily] = &[EvidenceFamily::TuningObservations];
fn tuning_budget() -> EvidenceReadBudget {
    EvidenceReadBudget { timeout_ms: 1000, ..Default::default() }
}

#[tokio::test]
async fn authenticated_tuning_readback_preserves_scope_counts_definitions_and_native_source_outcomes() {
    for (_, source, records) in test_support::tuning_evidence_cases() {
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::TuningObservations).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILIES, EvidenceReadBudget { page_rows: 1, ..tuning_budget() }, None)
            .await
            .unwrap();
        test_support::assert_tuning_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len());
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn changed_tuning_scopes_plan_ids_counts_keys_or_source_fail_the_original_warehouse_proof() {
    let (source, records) = test_support::tuning_evidence(test_support::TuningCase::Complete);
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::TuningObservations).await;
    let schema = fixture.catalog.get(TelemetryStream::TuningObservations).unwrap();
    let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    for (column, pointer, value) in [
        ("Details", "/plan_scope", json!({"kind":"replica-group","id":"99"})),
        ("Details", "/plan_id", json!("99")),
        ("Details", "/failures", json!("9007199254740992")),
        ("FactKey", "", json!("forced:3:9:replica:99")),
        ("NativeSource", "", json!("foreign-source")),
    ] {
        let mut rows = fixture.rows.clone();
        if column == "Details" {
            let mut data: Value = serde_json::from_str(rows[0][details].as_str().unwrap()).unwrap();
            *data.pointer_mut(pointer).unwrap() = value;
            rows[0][details] = json!(data.to_string());
        } else {
            let index = schema.columns().iter().position(|field| field.name == column).unwrap();
            rows[0][index] = value;
        }
        fixture.replies(vec![rows]);
        let error = fixture.reader.read_capture(&fixture.range, FAMILIES, tuning_budget(), None).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{column}{pointer}"
        );
    }
}

#[tokio::test]
async fn tuning_previews_stay_gaps_until_bounded_recovery_restores_the_exact_whole_batch() {
    for case in [test_support::TuningCase::LargeDefinition, test_support::TuningCase::LocalForcedRecovery] {
        let (source, records) = test_support::tuning_evidence(case);
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::TuningObservations).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let preview = fixture.reader.read_capture(&fixture.range, FAMILIES, tuning_budget(), None).await.unwrap();
        assert!(!preview.is_read_complete());
        assert!(preview.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        fixture.replies(vec![fixture.rows.clone()]);
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                FAMILIES,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..tuning_budget() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap();
        test_support::assert_tuning_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().recovery_objects, 1);
        assert_eq!(capture.metrics().recovery_bytes, fixture.range.manifest().batches[0].bytes);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before + 1);
    }
}
