use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn unresolved_native_rows_keep_physical_receipts_exact_counters_and_no_diagnostic_authority() {
    let (source, records) = test_support::informational_query_evidence();
    let fixture = Fixture::new_for_streams(
        source.clone(),
        records.clone(),
        &[TelemetryStream::QueryRuntime, TelemetryStream::QueryWaits],
    )
    .await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let pages = [TelemetryStream::QueryRuntime, TelemetryStream::QueryWaits]
        .into_iter()
        .flat_map(|stream| fixture.rows_for(stream).into_iter().map(|row| vec![row]))
        .collect();
    fixture.replies(pages);
    let captured = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::QueryRuntime, EvidenceFamily::QueryWaits],
            EvidenceReadBudget { page_rows: 1, ..Default::default() },
            None,
        )
        .await
        .unwrap();
    test_support::assert_informational_capture(&captured, &source, &records);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
    let semantic = schema.columns().iter().position(|column| column.name == "RecordId").unwrap();
    let rows = fixture.rows_for(TelemetryStream::QueryRuntime);
    assert!(rows[0][semantic].is_null());
    assert!(rows[1][semantic].is_null());
    assert!(rows[2][semantic].is_string());
    assert_eq!(Schema::column_type(&schema.columns()[semantic]), "Nullable(String)");
}

#[tokio::test]
async fn nullable_identity_coverage_counter_and_position_substitution_fail_original_projection_membership() {
    let (source, records) = test_support::informational_query_evidence();
    let fixture = Fixture::new_for_stream(source, records, TelemetryStream::QueryRuntime).await;
    let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
    for (field, value) in [
        ("RecordId", json!(digest(b"invented-semantic-id"))),
        ("RecordId", json!("")),
        ("ReferencesComplete", json!(true)),
        ("RowOrdinal", json!(2)),
        ("OwnerEpoch", json!(2)),
        ("Executions", json!("9007199254740992")),
        ("NativePayloadDigest", json!(digest(b"foreign-body"))),
    ] {
        let mut rows = fixture.rows_for(TelemetryStream::QueryRuntime);
        let column = schema.columns().iter().position(|column| column.name == field).unwrap();
        rows[0][column] = value;
        fixture.replies(vec![vec![rows.remove(0)]]);
        let error = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::QueryRuntime],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
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
    let fixture = Fixture::new_for_stream(source, records, TelemetryStream::QueryRuntime).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let rows = fixture.rows_for(TelemetryStream::QueryRuntime);
    fixture.replies(vec![vec![], vec![rows[1].clone()], vec![rows[2].clone()]]);
    let captured = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::QueryRuntime],
            EvidenceReadBudget { page_rows: 1, ..Default::default() },
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
