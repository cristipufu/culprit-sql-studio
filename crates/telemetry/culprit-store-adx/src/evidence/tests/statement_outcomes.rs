use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn column(fixture: &Fixture, name: &str) -> usize {
    fixture
        .catalog
        .get(TelemetryStream::OperationalSamples)
        .unwrap()
        .columns()
        .iter()
        .position(|column| column.name == name)
        .expect("declared operational column")
}

#[tokio::test]
async fn exact_statement_outcomes_and_node_bindings_replay_without_payload_recovery() {
    for (source, records) in test_support::statement_work_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_statement_work_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len());
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        for row in capture.records() {
            assert!(!row.native_payload_sha256().is_empty());
            assert!(row.row_ordinal() > 0);
        }
    }
}

#[tokio::test]
async fn a_missing_outcome_point_preserves_original_count_and_unknown_values_even_with_recovery_enabled() {
    let (source, records) = test_support::statement_outcome_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![vec![]]);
    let capture = fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.source(), &source);
    assert_eq!(source.observation.snapshot().unwrap().sources[0].records, 1);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let input = StatementCounterSnapshot::from_source(&source, vec![]).unwrap();
    assert!(input.is_valid_read(false) && !input.is_valid());
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn schema_count_unit_clock_and_scope_tampering_cannot_change_the_original_outcome_receipt() {
    let (source, records) = test_support::statement_outcome_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let details = column(&fixture, "Details");
    for (pointer, changed) in [
        ("/identity/qualifiers/schema", json!("another_schema")),
        ("/totals/1/total/value", json!("9007199254740992")),
        ("/totals/1/unit", json!({"kind":"bytes"})),
        ("/scope", json!("node")),
        ("/observed_at", json!("2020-01-01T11:59:00Z")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut body: Value = serde_json::from_str(rows[0][details].as_str().unwrap()).unwrap();
        *body.pointer_mut(pointer).expect("native counter field") = changed;
        rows[0][details] = json!(serde_json::to_string(&body).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(
                &fixture.range,
                FAMILY,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
