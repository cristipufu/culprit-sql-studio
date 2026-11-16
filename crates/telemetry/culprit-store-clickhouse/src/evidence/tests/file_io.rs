use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn counters(record: &TelemetryRecord) -> &FileIoCounters {
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::FileIo(file), .. }) = record
    else {
        panic!("native file-I/O point")
    };
    file
}

fn column(fixture: &Fixture, name: &str) -> usize {
    fixture
        .catalog
        .get(TelemetryStream::OperationalSamples)
        .unwrap()
        .columns()
        .iter()
        .position(|column| column.name == name)
        .expect("declared warehouse field")
}

#[tokio::test]
async fn native_file_counters_catalog_gauges_accounting_and_source_visibility_round_trip_without_payload_reads() {
    for (source, records) in test_support::file_io_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert_eq!(capture.source().observation.snapshot().unwrap().dataset, SnapshotDataset::Operational);
        assert_eq!(capture.metrics().pages as usize, records.len());
        for row in capture.records() {
            let native = counters(&row.row().record);
            assert_eq!(native.reset, culprit_core::ResetMarker::Unknown);
            assert!(native.sample_uptime_ms.is_none(), "no counter lifetime is invented from capture bounds");
            let original = records
                .iter()
                .map(counters)
                .find(|original| {
                    original.native_database_id == native.native_database_id && original.file_id == native.file_id
                })
                .unwrap();
            assert_eq!(native, original);
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(capture.metrics().recovery_bytes, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn a_missing_file_row_remains_a_gap_without_rewriting_native_counts_or_substituting_zero() {
    let (source, records) = test_support::file_io_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let mut rows = fixture.rows.clone();
    rows.remove(0);
    fixture.replies(vec![rows]);
    let capture =
        fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.source().observation.snapshot().unwrap().records as usize, records.len());
    assert_eq!(capture.records().len(), records.len() - 1);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    for actual in capture.records() {
        let expected = records
            .iter()
            .find(|record| {
                let candidate = counters(record);
                let retained = counters(&actual.row().record);
                candidate.native_database_id == retained.native_database_id && candidate.file_id == retained.file_id
            })
            .unwrap();
        assert_eq!(&actual.row().record, expected);
    }
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn altered_queue_gauges_native_scope_or_read_counts_fail_receipts_before_recovery_can_replace_them() {
    let (source, records) = test_support::file_io_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let details_column = column(&fixture, "Details");
    let selected = fixture
        .rows
        .iter()
        .position(|row| {
            let details: Value = serde_json::from_str(row[details_column].as_str().unwrap()).unwrap();
            details["queued_read_ms"] == json!("9007199254740993")
        })
        .expect("native large-queue fixture");
    for (pointer, replacement) in [
        ("/queued_read_ms", json!("9007199254740992")),
        ("/logical_size/bytes", json!("16777216")),
        ("/maximum_size/bytes", json!("4294967296")),
        ("/size_on_disk_bytes", json!("8388608")),
        ("/scope", json!("server")),
        ("/context/read", json!("cache-page")),
        ("/reads", json!("11")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[selected][details_column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).expect("retained native file field") = replacement;
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
