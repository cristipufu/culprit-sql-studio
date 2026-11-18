use super::*;
use culprit_core::{
    MeasurementScope, Metric, PerformanceCounterAvailability, PerformanceCounterEntry, PerformanceCounterReport,
    ResetMarker, SourceState, telemetry::*,
};

#[test]
fn compact_output_keeps_counts_and_exact_display_text_without_native_labels_or_raw_properties() {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    report.performance_counters = Some(Box::new(PerformanceCounterReport {
        context: None,
        native_source: None,
        source_state: Some(SourceState::Partial),
        source_reason: Some("private-native-failure-transcript".into()),
        read_complete: Some(false),
        native_observations: Some(1),
        observed_observations: 1,
        native_counter_rows: Some(12),
        observed_counter_rows: 12,
        presented_counter_rows: 12,
        entries_limited: false,
        fields_withheld: false,
        missing_fields: vec![Metric::text("missing field", "unknown")],
        title: "counter context".into(),
        summary: "partial".into(),
        limitations: vec!["shared server, no owner".into()],
        ratios: None,
        entries: (0..12)
            .map(|index| PerformanceCounterEntry {
                observation_key: "private-native-parent".into(),
                counter_key: format!("private-native-key-{index}"),
                field: "granted_workspace".into(),
                family: PerformanceCounterFamily::WorkspaceMemory,
                family_label: "workspace".into(),
                availability: PerformanceCounterAvailability::Observed,
                native_object: "private-native-object".into(),
                native_name: "private-native-name".into(),
                native_instance: Some("private-native-instance".into()),
                native_type: Some("65792".into()),
                observed_at: Some("2020-01-01T12:00:00Z".into()),
                scope: MeasurementScope::Server,
                nature: CounterNature::Gauge,
                unit: CounterUnit::Kibibytes,
                reset: ResetMarker::Unknown,
                raw_value: "9007199254740993".into(),
                metric: Metric::text("workspace", "9007199254740993 KiB"),
                explanation: "not pressure".into(),
            })
            .collect(),
    }));
    let mut response = json!({});
    attach(&mut response, &report);
    let counters = &response["performance_counters"];
    assert_eq!(counters["entries"].as_array().unwrap().len(), ENTRY_LIMIT);
    assert_eq!(counters["entries_limited"], true);
    assert_eq!(counters["native_counter_rows"], 12);
    assert_eq!(counters["presented_counter_rows"], 12);
    assert_eq!(counters["returned_counter_rows"], ENTRY_LIMIT);
    assert_eq!(counters["read_complete"], false);
    let text = serde_json::to_string(&response).unwrap();
    assert!(text.contains("9007199254740993 KiB"));
    assert!(!text.contains("private-native"));
    assert!(!text.contains("native_type"));
    report.performance_counters = None;
    report.snapshot.record_unavailable_evidence(
        culprit_core::source_id::PERFORMANCE_COUNTERS,
        culprit_core::message!("view.performance_counters.invalid"),
    );
    let mut response = json!({});
    attach(&mut response, &report);
    assert!(response.get("performance_counters_unavailable").is_some());
    assert!(response.get("performance_counters").is_none());
}
