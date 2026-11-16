use super::*;
use std::sync::atomic::Ordering;
use test_support::{ReplicationCase, assert_replication_capture, replication_evidence, replication_evidence_cases};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ReplicationSamples];

fn column(fixture: &Fixture, name: &str) -> usize {
    fixture
        .catalog
        .get(TelemetryStream::ReplicationSamples)
        .unwrap()
        .columns()
        .iter()
        .position(|column| column.name == name)
        .unwrap()
}

#[tokio::test]
async fn all_native_replication_sources_keep_exact_rows_receipts_counts_and_coverage_without_raw_object_reads() {
    for (source, records) in replication_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        assert_replication_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().selected_rows as usize, records.len());
        assert_eq!(capture.metrics().pages as usize, records.len());
        for actual in capture.records() {
            assert!(!actual.native_payload_sha256().is_empty());
            assert_eq!(actual.batch_id(), fixture.range.manifest().batches[0].batch_id);
            assert!(actual.row_ordinal() > 0);
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(capture.metrics().recovery_bytes, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn missing_replication_rows_preserve_the_original_source_population_and_never_supply_empty_or_zero() {
    let (source, records) = replication_evidence(ReplicationCase::PostgresSlots);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let mut rows = fixture.rows.clone();
    rows.remove(0);
    fixture.replies(vec![rows]);
    let capture =
        fixture.reader().read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.records().len(), records.len() - 1);
    assert_eq!(capture.metrics().selected_rows as usize, records.len());
    assert_eq!(capture.source().observation.snapshot().unwrap().records as usize, records.len());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    for row in capture.records() {
        assert!(records.contains(&row.row().record));
    }
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn changed_native_counter_position_generation_scope_or_context_fails_proof_before_optional_recovery() {
    for (case, changes) in [
        (
            ReplicationCase::PostgresSlots,
            vec![
                ("/measurements/counters/0/value/value", json!("9007199254740992")),
                ("/measurements/scope", json!("database")),
                ("/measurements/identity/qualifiers/native_id", json!("unrelated_slot")),
                ("/slot_name", json!("unrelated_slot")),
                ("/measurements/properties/synced", json!("true")),
            ],
        ),
        (
            ReplicationCase::PostgresSender,
            vec![
                ("/session/generation", json!("2026-11-15T00:00:00.000002Z")),
                ("/measurements/properties/reply_time", json!("2026-11-16T11:59:59.123457Z")),
            ],
        ),
        (
            ReplicationCase::MssqlAvailabilityGroup,
            vec![
                ("/last_sent_lsn", json!("1234567890123456789012341")),
                ("/last_sent_without_offset", json!("2026-11-16T01:02:03.127")),
                ("/group/name", json!("another_group")),
                ("/replica/role", json!(1)),
            ],
        ),
    ] {
        let (source, records) = replication_evidence(case);
        let fixture = Fixture::new(source, records).await;
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let details_column = column(&fixture, "Details");
        for (pointer, value) in changes {
            let mut rows = fixture.rows.clone();
            let mut details: Value = serde_json::from_str(rows[0][details_column].as_str().unwrap()).unwrap();
            *details.pointer_mut(pointer).expect("authentic native field") = value;
            rows[0][details_column] = json!(serde_json::to_string(&details).unwrap());
            fixture.replies(vec![rows]);
            let error = fixture
                .reader()
                .read_capture(
                    &fixture.range,
                    FAMILY,
                    EvidenceReadBudget {
                        recovery_payloads: Some(RecoveryPayloadBudget::default()),
                        ..Default::default()
                    },
                    Some(fixture.recovery.as_ref()),
                )
                .await
                .unwrap_err();
            assert_eq!(
                error.detail,
                Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
                "{case:?}: {pointer}"
            );
            assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        }
        for (name, value) in [
            ("FactKey", json!("another-native-reference")),
            ("NativeSource", json!("another-native-source")),
            ("SourceId", json!("another-source-scope")),
        ] {
            let mut rows = fixture.rows.clone();
            rows[0][column(&fixture, name)] = value;
            fixture.replies(vec![rows]);
            let error = fixture
                .reader()
                .read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None)
                .await
                .unwrap_err();
            assert_eq!(
                error.detail,
                Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
                "{case:?}: {name}"
            );
        }
    }
}
