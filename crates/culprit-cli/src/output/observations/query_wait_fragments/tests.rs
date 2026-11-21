use super::*;
use culprit_collector::analytical::test_support::query_wait_fragment_evidence;
use culprit_core::telemetry::*;
fn report() -> QueryWaitFragmentReport {
    let (source, records) = query_wait_fragment_evidence();
    let capture = CaptureIdentity {
        observation_id: "original-waits".into(),
        revision: Some(CaptureRevision {
            outbox_id: "original-wait-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let read = CapturedReadRef::new(
        &source,
        capture.clone(),
        digest(b"original-manifest"),
        vec![CapturedStreamRows { stream: TelemetryStream::QueryWaits, rows: 2 }],
    )
    .unwrap();
    let exports = records
        .into_iter()
        .filter(|row| matches!(row, TelemetryRecord::QueryWaits(_)))
        .enumerate()
        .map(|(ordinal, record)| (ExportedRecord::new(&source, &capture, record).unwrap(), ordinal as u32))
        .collect::<Vec<_>>();
    let original = CapturedQueryHistoryRead::from_exports(
        source,
        read,
        ReferenceCoverage::default(),
        ("2020-01-02T00:00:00Z".into(), "2020-01-02T00:00:01Z".into()),
        exports,
        true,
    )
    .unwrap();
    culprit_checks::query_wait_fragments::explain(&original).unwrap()
}
#[test]
fn human_wait_output_keeps_each_native_fragment_and_every_scope_distribution_and_history_qualification() {
    let report = report();
    let text = query_wait_fragments(&report);
    assert!(text.contains(&report.title));
    assert!(text.contains(&report.summary));
    for entry in &report.entries {
        for metric in &entry.metrics {
            assert!(text.contains(metric.text.as_deref().unwrap()));
        }
    }
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    let compact = compact_query_wait_fragments(&report);
    assert_eq!(compact["entries_presented"], 2);
    assert_eq!(compact["historical_context"], true);
    assert_eq!(compact["references_complete"], report.references_complete.unwrap());
    crate::test_support::assert_no_json_keys(
        &compact,
        &["manifest_sha256", "source_sha256", "payload_sha256", "references", "qualifiers"],
    );
}
#[test]
fn native_labels_are_bounded_escaped_and_unavailable_history_is_not_zero_measured_wait() {
    let mut report = report();
    report.entries[0].category_description = Some("native\nlabel\u{1b}[31m😀".repeat(70));
    let text = query_wait_fragments(&report);
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains("native\nlabel"));
    assert!(text.contains(&statement::preview(report.entries[0].category_description.as_deref().unwrap())));
    let unavailable = culprit_checks::query_wait_fragments::unavailable(&culprit_core::message!(
        "view.query_wait_fragments.sharing_withheld"
    ));
    let text = query_wait_fragments(&unavailable);
    assert!(text.contains(&unavailable.summary));
    assert!(text.contains(unavailable.source_reason.as_deref().unwrap()));
    assert_eq!(compact_query_wait_fragments(&unavailable)["selected_rows"], serde_json::Value::Null);
}
