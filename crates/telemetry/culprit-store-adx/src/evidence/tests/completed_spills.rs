use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn completed_spill_and_definition_rows_keep_original_proofs_without_implicit_recovery() {
    for case in test_support::completed_spill_evidence_cases() {
        let short = case.name == "short_unicode_sql";
        for (definition, (source, records)) in
            std::iter::once((false, case.runtime)).chain(case.definitions.map(|input| (true, input)))
        {
            let family =
                if definition { EvidenceFamily::ObjectDefinitions } else { EvidenceFamily::OperationalSamples };
            let fixture = Fixture::new(source.clone(), records.clone()).await;
            let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
            assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
            fixture.reply_families(&[family], 250);
            let capture = fixture.reader().read_capture(&fixture.range, &[family], budget(), None).await.unwrap();
            if short && definition {
                assert!(!capture.is_read_complete());
                assert!(capture.records().is_empty());
                assert!(!capture.gaps().is_empty());
            } else {
                test_support::assert_operational_capture(&capture, &source, &records);
            }
            assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
            assert_eq!(capture.metrics().recovery_objects, 0);
            assert_eq!(capture.source(), &source);
            if short && definition {
                *fixture.recovery.fault.lock().unwrap() = Fault::None;
                fixture.reply_families(&[family], 250);
                let batch = &fixture.range.manifest().batches[0];
                let hydration = EvidenceReadBudget {
                    recovery_payloads: Some(RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes }),
                    ..budget()
                };
                let hydrated = fixture
                    .reader()
                    .read_capture(&fixture.range, &[family], hydration, Some(fixture.recovery.as_ref()))
                    .await
                    .unwrap();
                test_support::assert_operational_capture(&hydrated, &source, &records);
                assert_eq!(hydrated.metrics().recovery_objects, 1);
                assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens + 1);
            }
        }
    }
}

#[tokio::test]
async fn changing_completed_spill_totals_or_native_incarnation_fails_the_original_projection_receipt() {
    for field in ["total", "sql_handle", "created_without_offset"] {
        let case = test_support::completed_spill_evidence_cases()
            .into_iter()
            .find(|case| case.name == "recorded_spills")
            .unwrap();
        let fixture = Fixture::new(case.runtime.0, case.runtime.1).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows_for(EvidenceFamily::OperationalSamples);
        let mut details: serde_json::Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        match field {
            "total" => details["counters"]["spilled-pages"]["total"] = json!("0"),
            "sql_handle" => details["context"]["identity"]["sql_handle"] = json!("0x9999"),
            _ => details["context"]["identity"]["created_without_offset"] = json!("2020-01-01T11:00:01.003"),
        }
        rows[0][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget(), None)
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn a_missing_native_spill_row_remains_a_gap_with_the_original_population_count() {
    let case =
        test_support::completed_spill_evidence_cases().into_iter().find(|case| case.name == "recorded_spills").unwrap();
    let source = case.runtime.0.clone();
    let fixture = Fixture::new(case.runtime.0, case.runtime.1).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    fixture.replies(vec![vec![]]);
    let capture = fixture
        .reader()
        .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget(), None)
        .await
        .unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.source().observation.snapshot().unwrap().records, 1);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    assert_eq!(capture.metrics().recovery_objects, 0);
}
