use super::*;
use culprit_core::{CounterRatioEntry, CounterRatioReport, MeasurementScope, Metric, SourceState, telemetry::*};

fn report() -> Report {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    report.counter_ratios = Some(Box::new(CounterRatioReport {
        source_state: Some(SourceState::Partial),
        source_reason: Some("private-native-failure-transcript".into()),
        read_complete: false,
        declared_ratios: 25,
        presented_ratios: 25,
        entries_limited: false,
        title: culprit_core::t!("view.counter_ratios.title"),
        summary: culprit_core::t!("view.counter_ratios.summary.partial"),
        limitations: vec![culprit_core::t!("view.counter_ratios.limit.aggregate")],
        entries: (0..25)
            .map(|n| CounterRatioEntry {
                observation_key: format!("private-native-reference-{n}"),
                kind: CounterRatioKind::LocalSsdCacheHit,
                scope: MeasurementScope::Server,
                observed_at: Some("2020-01-01T12:00:00.5Z".into()),
                native_object: Some("private-native-counter-owner".into()),
                native_instance: Some("private-native-instance".into()),
                issues: vec![CounterRatioIssue::MissingBase],
                title: culprit_core::t!("view.counter_ratios.local_ssd.title"),
                explanation: culprit_core::t!("view.counter_ratios.issue.missing_base"),
                metrics: vec![Metric::text(
                    culprit_core::t!("view.counter_ratios.metric.numerator"),
                    "9007199254740993",
                )],
            })
            .collect(),
    }));
    report.snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![NativeCounterSet {
        identity: NativeIdentity::new("private-source-body-identity"),
        scope: MeasurementScope::Server,
        counters: vec![],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: [("sql".into(), "SELECT private_native_password_transcript".into())].into(),
    }]);
    report
}

#[test]
fn compact_counter_view_is_bounded_keeps_exact_text_and_does_not_dump_private_raw_owner_or_body() {
    let mut response = json!({});
    attach(&mut response, &report());
    let ratio = &response["counter_ratios"];
    assert_eq!(ratio["entries"].as_array().unwrap().len(), ENTRY_LIMIT);
    assert_eq!(ratio["declared_ratios"], 25);
    assert_eq!(ratio["presented_ratios"], ENTRY_LIMIT);
    assert_eq!(ratio["entries_limited"], true);
    assert_eq!(ratio["source_state"], "partial");
    assert_eq!(ratio["read_complete"], false);
    let text = serde_json::to_string(&response).unwrap();
    assert!(text.contains("9007199254740993"));
    for private in
        ["private-native-", "private-source-body-identity", "private_native_password_transcript", "fixture.invalid"]
    {
        assert!(!text.contains(private), "{private}");
    }
}
