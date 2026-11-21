use super::*;
use culprit_collector::analytical::test_support::query_runtime_fragment_evidence;
use culprit_core::telemetry::*;
fn report() -> QueryRuntimeFragmentReport {
    let (source, records) = query_runtime_fragment_evidence(true);
    let capture = CaptureIdentity {
        observation_id: "original".into(),
        revision: Some(CaptureRevision { outbox_id: "outbox".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let read = CapturedReadRef::new(
        &source,
        capture.clone(),
        digest(b"manifest"),
        vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows: 2 }],
    )
    .unwrap();
    let rows = records
        .into_iter()
        .filter(|row| matches!(row, TelemetryRecord::QueryRuntime(_)))
        .enumerate()
        .map(|(ordinal, row)| (ExportedRecord::new(&source, &capture, row).unwrap(), ordinal as u32))
        .collect::<Vec<_>>();
    let read = CapturedQueryHistoryRead::from_exports(
        source,
        read,
        ReferenceCoverage::default(),
        ("2020-01-02T00:00:00Z".into(), "2020-01-02T00:00:01Z".into()),
        rows,
        true,
    )
    .unwrap();
    culprit_checks::query_runtime_fragments::explain(&read).unwrap()
}
#[test]
fn human_runtime_output_preserves_exact_endpoints_unknown_mean_all_present_metrics_and_native_qualifications() {
    let report = report();
    let text = runtime_fragments(&report);
    assert!(text.contains(&report.title));
    assert!(text.contains(&report.summary));
    for entry in &report.entries {
        for metric in &entry.metrics {
            for statistic in &metric.statistics {
                assert!(text.contains(statistic.text.as_deref().unwrap()));
            }
        }
    }
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(&t!("view.runtime_fragments.unknown")));
    let compact = compact_runtime_fragments(&report);
    assert_eq!(compact["entries_presented"], 2);
    assert_eq!(compact["entries"][0]["executions"], "2");
    crate::test_support::assert_no_json_keys(
        &compact,
        &["manifest_sha256", "source_sha256", "qualifiers", "payload_sha256"],
    );
}
#[test]
fn withdrawn_runtime_originals_are_explicitly_unavailable_without_fake_zero_statistics() {
    let report = culprit_checks::query_runtime_fragments::unavailable(&culprit_core::message!(
        "view.runtime_fragments.sharing_withheld"
    ));
    let text = runtime_fragments(&report);
    assert!(text.contains(&report.summary));
    let compact = compact_runtime_fragments(&report);
    assert_eq!(compact["selected_rows"], serde_json::Value::Null);
    assert_eq!(compact["entries_presented"], 0);
    assert_eq!(compact["source_state"], "unavailable");
}
