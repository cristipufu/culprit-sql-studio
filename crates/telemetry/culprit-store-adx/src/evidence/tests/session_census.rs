use super::*;
use std::sync::atomic::Ordering;
use test_support::SessionCensusCase;

async fn collected(case: SessionCensusCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let (source, rows) = test_support::session_census_evidence(case);
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("adx:test", catalog.digest(), source.clone(), rows.clone()).await;
    (Fixture::from_recovery(catalog, Arc::new(recovery)).await, source, rows)
}
#[tokio::test]
async fn collected_sealed_restarted_published_census_retains_native_session_request_relationships_and_exact_fields() {
    for case in SessionCensusCase::ALL {
        let (fixture, source, rows) = collected(case).await;
        fixture.replies(if fixture.rows.is_empty() { vec![] } else { vec![fixture.rows.clone()] });
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::ActivityObservations], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_activity_capture(&capture, &source, &rows);
        assert!(capture.is_read_complete());
        assert_eq!(capture.captured_rows(TelemetryStream::ActivityObservations), Some(rows.len() as u32));
        for (actual, native) in capture.records().iter().zip(&rows) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), native.clone()).unwrap());
        }
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
#[tokio::test]
async fn missing_session_bodies_keep_original_mars_population_and_never_requery_or_hydrate() {
    let (fixture, source, _) = collected(SessionCensusCase::Mars).await;
    fixture.replies(vec![vec![]]);
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let capture = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::ActivityObservations],
            EvidenceReadBudget::default(),
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.captured_rows(TelemetryStream::ActivityObservations), Some(2));
    assert_eq!(capture.source(), &source);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}
#[tokio::test]
async fn altered_client_lifetimes_large_totals_nullable_fields_or_connection_id_cannot_borrow_native_receipts() {
    for (pointer, value) in [
        ("/session_context/client_library", json!("another-library")),
        ("/session_context/native/data/request_connection_id", json!("33333333-3333-3333-3333-333333333333")),
        ("/session_context/native/data/times/login_without_offset", json!("2020-01-01T11:50:01.0000000")),
        ("/session_context/native/data/tds_protocol_version", json!(0)),
        ("/session_counters/logical_reads", json!("9007199254740992")),
        ("/session_counters/cpu_ms", Value::Null),
    ] {
        let (fixture, _, _) = collected(SessionCensusCase::Mars).await;
        let column = fixture
            .catalog
            .get(TelemetryStream::ActivityObservations)
            .unwrap()
            .columns()
            .iter()
            .position(|column| column.name == "ActivityFacts")
            .unwrap();
        let mut rows = fixture.rows.clone();
        let mut detail: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *detail.pointer_mut(pointer).unwrap() = value;
        rows[0][column] = json!(serde_json::to_string(&detail).unwrap());
        fixture.replies(vec![rows]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        assert!(
            fixture
                .reader()
                .read_capture(
                    &fixture.range,
                    &[EvidenceFamily::ActivityObservations],
                    EvidenceReadBudget::default(),
                    Some(fixture.recovery.as_ref())
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
