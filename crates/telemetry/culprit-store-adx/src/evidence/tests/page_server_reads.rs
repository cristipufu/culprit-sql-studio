use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn page_server_runtime_fragments_keep_native_precision_scope_and_missing_values_without_recovery() {
    for case in test_support::page_server_read_evidence_cases() {
        let fixture = Fixture::new(case.source.clone(), case.records.clone()).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        fixture.reply_families(&[EvidenceFamily::QueryRuntime], 1);
        let capture = fixture
            .reader()
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::QueryRuntime],
                EvidenceReadBudget { page_rows: 1, ..budget() },
                None,
            )
            .await
            .unwrap();
        test_support::assert_page_server_read_capture(&capture, &case.source, &case.records);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens, "{}", case.name);
        if case.name == "native_fragment" {
            let TelemetryRecord::QueryRuntime(row) = &capture.records()[0].row().record else { panic!() };
            let native = row.page_server_reads.as_ref().unwrap();
            assert_eq!(native.last_pages.as_deref(), Some("9007199254740993"));
            assert_eq!(native.maximum_pages.as_deref(), Some(i64::MAX.to_string().as_str()));
            assert_eq!(native.average_pages, Some(12.5));
            assert_eq!(native.standard_deviation_pages, Some(1.25));
        }
    }
}

#[tokio::test]
async fn page_server_body_native_population_clock_and_namespace_tampering_fail_original_receipts() {
    let case = test_support::page_server_read_evidence_cases().remove(0);
    let fixture = Fixture::new(case.source.clone(), case.records.clone()).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
    for (name, pointer, changed) in [
        ("PageServerReads", "/last_pages", json!("0")),
        ("PageServerReads", "/average_pages", json!(0.0)),
        ("PageServerReads", "/availability", json!("not-applicable")),
        ("PageServerReads", "", Value::Null),
        ("NativeDimensions", "/replica_group_id", json!("2")),
        ("NativeDimensions", "/execution_type", json!("3")),
        ("NativeDimensions", "/plan_id", json!("5")),
        ("FirstExecutionAt", "", json!("2020-01-01T06:02:00Z")),
        ("ResourceId", "", json!(digest(b"foreign database namespace"))),
    ] {
        let column = schema.columns().iter().position(|column| column.name == name).unwrap();
        let mut rows = fixture.rows_for(EvidenceFamily::QueryRuntime);
        if pointer.is_empty() {
            rows[0][column] = changed;
        } else {
            let mut body: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
            *body.pointer_mut(pointer).unwrap() = changed;
            rows[0][column] = json!(serde_json::to_string(&body).unwrap());
        }
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::QueryRuntime],
                EvidenceReadBudget { page_rows: 1, ..budget() },
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{name}{pointer}"
        );
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn missing_remote_read_fragment_keeps_original_selected_count_and_explicit_gap() {
    let case = test_support::page_server_read_evidence_cases().remove(0);
    let fixture = Fixture::new(case.source.clone(), case.records.clone()).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![vec![]]);
    let capture = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::QueryRuntime],
            EvidenceReadBudget { page_rows: 1, ..budget() },
            None,
        )
        .await
        .unwrap();
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &case.source);
    assert_eq!(capture.metrics().selected_rows, 1);
    assert!(capture.records().is_empty());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    assert_eq!(capture.metrics().recovery_objects, 0);
}
