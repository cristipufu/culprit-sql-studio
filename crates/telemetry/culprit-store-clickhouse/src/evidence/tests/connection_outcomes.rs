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
        .unwrap()
}

#[tokio::test]
async fn native_outcome_counts_clocks_scopes_layout_and_unrelated_source_rows_round_trip_without_object_reads() {
    for (source, records) in test_support::connection_outcome_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_connection_outcome_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(capture.metrics().recovery_bytes, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn missing_mapped_or_unrelated_rows_preserve_original_counts_as_archive_gaps() {
    let (source, records) = test_support::connection_outcome_evidence_cases().remove(0);
    for missing in 0..records.len() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let mut rows = fixture.rows.clone();
        rows.remove(missing);
        fixture.replies(vec![rows]);
        let capture =
            fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
        assert!(!capture.is_read_complete());
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.records().len(), records.len() - 1);
        assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
        let points = capture
            .records()
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(row) = &record.row().record else {
                    panic!("original operational counter")
                };
                row.clone()
            })
            .collect();
        let snapshot = culprit_core::ConnectionOutcomeSnapshot::from_source(capture.source(), points).unwrap();
        assert!(snapshot.is_valid_read(false));
        assert!(!snapshot.is_valid());
        assert_eq!(snapshot.sources, source.observation.snapshot().unwrap().sources);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn altered_native_integer_reset_source_clock_scope_or_generation_fails_receipts_without_recovery() {
    let (source, records) = test_support::connection_outcome_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let details_column = column(&fixture, "Details");
    let selected = fixture
        .rows
        .iter()
        .position(|row| {
            let details: Value = serde_json::from_str(row[details_column].as_str().unwrap()).unwrap();
            details["identity"]["id"] == json!("pg_stat_database")
        })
        .unwrap();
    for (pointer, replacement) in [
        ("/counters/1/value/value", json!("9007199254740992")),
        ("/counters/1/reset", json!({"kind":"unknown"})),
        ("/properties/counter_observed_at", json!("2030-01-02T12:00:00Z")),
        ("/identity/qualifiers/dbid", json!("16385")),
        ("/identity/qualifiers/postmaster_started_at", json!("2020-01-02T12:00:00Z")),
        ("/scope", json!("server")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[selected][details_column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = replacement;
        rows[selected][details_column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(
                &fixture.range,
                FAMILY,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened, "{pointer}");
    }
}
