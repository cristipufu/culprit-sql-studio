use super::*;
use std::sync::atomic::Ordering;
use test_support::{JobsCase, assert_jobs_capture, jobs_evidence, jobs_evidence_cases};
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
#[tokio::test]
async fn job_views_run_nulls_lifetime_counters_and_original_receipts_round_trip_without_sql_recovery() {
    for (source, records) in jobs_evidence_cases() {
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::OperationalSamples).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        assert_jobs_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().selected_rows as usize, records.len());
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
#[tokio::test]
async fn missing_job_rows_keep_original_header_counts_and_cannot_supply_complete_or_empty_history() {
    let (source, records) = jobs_evidence(JobsCase::Failed);
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::OperationalSamples).await;
    let mut rows = fixture.rows.clone();
    rows.remove(0);
    fixture.replies(vec![rows]);
    let capture =
        fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.records().len(), records.len() - 1);
    assert_eq!(capture.source().observation.snapshot().unwrap().records as usize, records.len());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    for actual in capture.records() {
        assert!(records.contains(&actual.row().record));
    }
}
#[tokio::test]
async fn altered_native_outcome_clock_or_reported_label_is_rejected_before_any_recovery_access() {
    let (source, records) = jobs_evidence(JobsCase::Failed);
    let fixture = Fixture::new_for_stream(source, records, TelemetryStream::OperationalSamples).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let kind = schema.columns().iter().position(|column| column.name == "FactKind").unwrap();
    let index = fixture.rows.iter().position(|row| row[kind] == json!("timescale-job-run")).unwrap();
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    for (pointer, value) in [
        ("/succeeded", json!(true)),
        ("/started_at", json!("2026-11-15T11:00:00.000002Z")),
        ("/procedure/label/name", json!("another_reported_label")),
        ("/native_run_id", json!("9007199254740992")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[index][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[index][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error =
            fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
