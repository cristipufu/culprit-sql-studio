use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn unresolved_native_rows_keep_physical_receipts_exact_counters_and_no_diagnostic_authority() {
    let (source, records) = test_support::informational_query_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let families = [EvidenceFamily::QueryRuntime, EvidenceFamily::QueryWaits];
    fixture.reply_families(&families, 1);
    let captured = fixture
        .reader()
        .read_capture(&fixture.range, &families, EvidenceReadBudget { page_rows: 1, ..budget() }, None)
        .await
        .unwrap();
    test_support::assert_informational_capture(&captured, &source, &records);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
    let presence = schema.columns().iter().position(|column| column.name == "HasRecordId").unwrap();
    let semantic = schema.columns().iter().position(|column| column.name == "RecordId").unwrap();
    let rows = fixture.rows_for(EvidenceFamily::QueryRuntime);
    assert_eq!(rows[0][presence], false);
    assert_eq!(rows[0][semantic], "");
    assert_eq!(rows[1][presence], false);
    assert_eq!(rows[2][presence], true);
}

#[tokio::test]
async fn presence_coverage_counter_and_position_substitution_fail_original_projection_membership() {
    let (source, records) = test_support::informational_query_evidence();
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
    for (field, value) in [
        ("HasRecordId", json!(true)),
        ("RecordId", json!(digest(b"invented-semantic-id"))),
        ("ReferencesComplete", json!(true)),
        ("RowOrdinal", json!(2)),
        ("OwnerEpoch", json!(2)),
        ("Executions", json!("9007199254740992")),
        ("NativePayloadDigest", json!(digest(b"foreign-body"))),
    ] {
        let mut rows = fixture.rows_for(EvidenceFamily::QueryRuntime);
        let column = schema.columns().iter().position(|column| column.name == field).unwrap();
        rows[0][column] = value;
        fixture.replies(vec![vec![rows.remove(0)]]);
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
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{field}");
    }
}

#[tokio::test]
async fn missing_unidentified_body_remains_a_gap_with_original_count_and_no_recovery_open() {
    let (source, records) = test_support::informational_query_evidence();
    let fixture = Fixture::new(source, records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let rows = fixture.rows_for(EvidenceFamily::QueryRuntime);
    fixture.replies(vec![vec![], vec![rows[1].clone()], vec![rows[2].clone()]]);
    let captured = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::QueryRuntime],
            EvidenceReadBudget { page_rows: 1, ..budget() },
            None,
        )
        .await
        .unwrap();
    assert!(!captured.is_read_complete());
    assert_eq!(captured.captured_rows(TelemetryStream::QueryRuntime), Some(3));
    assert_eq!(captured.records().len(), 2);
    assert!(
        captured.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows && gap.row_ordinal == Some(1))
    );
    assert!(!captured.reference_coverage().complete);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    assert_eq!(captured.metrics().recovery_objects, 0);
}
