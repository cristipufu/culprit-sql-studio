use super::*;
use culprit_collector::analytical::test_support::{ChangeFeedCase, change_feed_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn local_mcp_change_feed_uses_original_source_rows_limits_unknowns_and_references_without_live_calls() {
    for case in ChangeFeedCase::ALL {
        let (source, rows) = change_feed_evidence(case);
        let expected = source.observation.snapshot().unwrap().clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::ChangeFeed).await;
        assert!(!error, "{case:?}:{value}");
        assert_eq!(value["analysis"], "change_feed");
        assert_eq!(value["native_rows"], expected.records);
        assert_eq!(value["native_sources"].as_array().unwrap().len(), 3);
        assert_eq!(value["read_complete"], true);
        assert_eq!(value["original_read_bounds"]["from"], expected.observed_from);
        assert_eq!(value["original_read_bounds"]["until"], expected.observed_until);
        assert_eq!(value["historical_context"], true);
        assert!(value["change_feed"].get("enabled").is_none());
        assert!(value["change_feed"].get("new_errors").is_none());
        for entry in value["change_feed"]["entries"].as_array().unwrap() {
            assert_eq!(entry["reference"].as_str().unwrap().len(), 64);
            assert!(entry["fields"].as_array().unwrap().len() <= 8);
        }
        if case == ChangeFeedCase::NativeNulls {
            assert!(value["change_feed"]["entries"][0]["fields"][2]["value"].is_null());
        }
        for private in [
            "error_message",
            "00000016:00000058:0001",
            "manifest_sha256",
            "source_sha256",
            "qualifiers",
            "original-instance",
            "original.invalid",
        ] {
            assert!(!value.to_string().contains(private), "{case:?}:{private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn local_mcp_change_feed_retains_original_counts_when_the_compact_entry_and_field_caps_apply() {
    let (mut source, records) = change_feed_evidence(ChangeFeedCase::ProgressAndError);
    let template = records
        .iter()
        .find(|r| matches!(r,TelemetryRecord::Operational(row) if row.source==MSSQL_CHANGE_FEED_ERRORS_SOURCE))
        .unwrap()
        .clone();
    let mut rows = records
        .into_iter()
        .filter(|r| !matches!(r,TelemetryRecord::Operational(row) if row.source==MSSQL_CHANGE_FEED_ERRORS_SOURCE))
        .collect::<Vec<_>>();
    for position in 1..=7 {
        let mut row = template.clone();
        let TelemetryRecord::Operational(record) = &mut row else { unreachable!() };
        let OperationalFact::MssqlChangeFeed(point) = &mut record.fact else { unreachable!() };
        point.position = position;
        record.key = point.key().unwrap();
        rows.push(row);
    }
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    header.records = rows.len() as u32;
    header.sources[2].records = 7;
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::ChangeFeed).await;
    assert!(!error, "{value}");
    assert_eq!(value["change_feed"]["entries_available"], 9);
    assert_eq!(value["change_feed"]["entries_presented"], 5);
    assert_eq!(value["change_feed"]["entries_omitted"], 4);
    assert_eq!(value["native_rows"], 10);
    assert_eq!(value["change_feed"]["entries"][0]["fields_available"], 9);
    assert_eq!(value["change_feed"]["entries"][0]["fields_omitted"], 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
