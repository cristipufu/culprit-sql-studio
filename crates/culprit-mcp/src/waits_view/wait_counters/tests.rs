use super::*;
use culprit_core::{MeasurementScope, Metric, ResetMarker, SourceOutcome, SourceState, WaitCounterEntry, telemetry::*};
fn wait_report() -> WaitCounterReport {
    WaitCounterReport {
        context: Some(ObservationReadContext::Collector {
            observed_from: "2026-11-21T00:00:01.000000001Z".into(),
            observed_until: "2026-11-21T00:00:02.000000009Z".into(),
        }),
        sources: vec![SourceOutcome {
            source: "waits".into(),
            state: SourceState::Partial,
            reason: None,
            reason_message: None,
        }],
        read_complete: false,
        native_rows: 30,
        observed_rows: 30,
        presented_rows: 30,
        entries_limited: false,
        title: culprit_core::t!("view.wait_counters.title"),
        summary: culprit_core::t!("view.wait_counters.summary.partial"),
        limitations: vec![culprit_core::t!("view.pool_waits.original")],
        entries: (0..30)
            .map(|index| WaitCounterEntry {
                observation: WaitCounterObservation {
                    source: "waits".into(),
                    wait_type: format!("NATIVE_{index}"),
                    mechanism: None,
                    scope: MeasurementScope::Pool,
                    reset: ResetMarker::Unknown,
                    waiting_tasks_count: "9007199254740993".into(),
                    wait_time_ms: "7".into(),
                    signal_wait_time_ms: "2".into(),
                    max_wait_time_ms: "4".into(),
                },
                resource_wait_time_ms: Some("5".into()),
                issues: vec![],
                title: format!("NATIVE_{index}"),
                explanation: culprit_core::t!("view.wait_counters.entry.explanation"),
                metrics: vec![Metric::text("native", "9007199254740993")],
            })
            .collect(),
    }
}
#[test]
fn compact_waits_keep_original_pool_scope_exact_counts_clocks_partiality_and_bounded_entries() {
    let report = wait_report();
    let value = value(&report);
    assert_eq!(value["entries"].as_array().unwrap().len(), ENTRY_LIMIT);
    assert_eq!(value["native_rows"], 30);
    assert_eq!(value["entries_limited"], true);
    assert_eq!(value["sources"][0]["state"], "partial");
    assert_eq!(value["read_complete"], false);
    assert_eq!(value["entries"][0]["scope"], "pool");
    assert_eq!(value["entries"][0]["waiting_tasks_count"], "9007199254740993");
    assert_eq!(value["entries"][0]["reset"]["kind"], "unknown");
    assert_eq!(value["original_read_bounds"]["until"], "2026-11-21T00:00:02.000000009Z");
    assert_eq!(value["limitations"][0], report.limitations[0]);
    assert!(value["context"].is_null());
    assert!(value["entries"][0]["reference"].is_null());
}
#[test]
fn missing_wait_explanation_retains_unavailable_marker_in_normal_mcp_waits_instead_of_silence() {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    report.wait_counters = Some(Box::new(wait_report()));
    let mut response = json!({});
    attach(&mut response, &report);
    assert!(response["wait_counters"].is_object());
    report.wait_counters = None;
    report.snapshot.record_unavailable_evidence(
        culprit_core::source_id::WAITS,
        culprit_core::message!("view.wait_counters.invalid"),
    );
    let mut response = json!({});
    attach(&mut response, &report);
    assert_eq!(response["wait_counters_unavailable"], culprit_core::t!("view.wait_counters.invalid"));
    assert!(response["wait_counters"].is_null());
}
