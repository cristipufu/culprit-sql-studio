use super::*;
use culprit_collector::analytical::test_support::{CdcCase, cdc_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn cdc_local_mcp_action_reuses_original_native_outcomes_without_source_calls_or_private_payloads() {
    for case in CdcCase::ALL {
        let (source, records) = cdc_evidence(case);
        let expected = source.observation.snapshot().unwrap().clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, records).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Cdc).await;
        assert!(!error, "{case:?}: {value}");
        assert_eq!(value["analysis"], "cdc");
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["native_rows"], expected.records);
        assert_eq!(value["native_sources"].as_array().unwrap().len(), 3);
        assert_eq!(value["read_complete"], true);
        assert_eq!(
            value["cdc"]["enabled"],
            if case == CdcCase::Disabled {
                json!(false)
            } else if case == CdcCase::UnknownFlag {
                serde_json::Value::Null
            } else {
                json!(true)
            }
        );
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
async fn cdc_local_mcp_caps_original_errors_at_five_without_new_error_or_event_feed_claims() {
    let (mut source, records) = cdc_evidence(CdcCase::ErrorAndProgress);
    let template = records
        .iter()
        .find(|record| matches!(record,TelemetryRecord::Operational(row)if row.source==MSSQL_CDC_ERRORS_SOURCE))
        .unwrap()
        .clone();
    let mut rows = records
        .into_iter()
        .filter(|record| !matches!(record,TelemetryRecord::Operational(row)if row.source==MSSQL_CDC_ERRORS_SOURCE))
        .collect::<Vec<_>>();
    for position in 1..=7 {
        let mut row = template.clone();
        let TelemetryRecord::Operational(record) = &mut row else { unreachable!() };
        let OperationalFact::MssqlCdc(point) = &mut record.fact else { unreachable!() };
        point.position = position;
        record.key = point.key().unwrap();
        rows.push(row);
    }
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    header.records = rows.len() as u32;
    header.sources.iter_mut().find(|source| source.source == MSSQL_CDC_ERRORS_SOURCE).unwrap().records = 7;
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Cdc).await;
    assert!(!error, "{value}");
    assert_eq!(value["cdc"]["entries_available"], 8);
    assert_eq!(value["cdc"]["entries_presented"], 5);
    assert_eq!(value["cdc"]["entries_omitted"], 3);
    assert_eq!(value["native_rows"], 9);
    assert!(value["cdc"].get("new_errors").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
