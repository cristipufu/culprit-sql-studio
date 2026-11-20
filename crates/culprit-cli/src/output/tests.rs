use super::*;

fn report() -> Report {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.report.json"))
        .expect("fixture");
    serde_json::from_str(&text).expect("report")
}

#[test]
fn stopped_findings_get_their_own_section_and_text_stays_text() {
    let mut r = report();
    let context = r.findings.iter().filter(|f| !f.is_actionable()).count();
    r.findings[0].inactive = true;
    r.findings[1].title = "orders <img src=x onerror=alert(1)> | split".into();
    let md = markdown_report(&r);
    assert!(md.contains(&format!("## Information and history ({})", context + 1)), "{md}");
    assert!(md.contains("&lt;img") && !md.contains("<img"));
    assert!(md.contains("\\| split"));
}

#[test]
fn compact_saved_scan_json_preserves_both_counter_populations_without_private_originals() {
    let mut report = report();
    let (source, records) = culprit_collector::analytical::test_support::performance_counter_evidence(
        culprit_collector::analytical::test_support::PerformanceCounterCase::Exact,
    );
    let coverage = source
        .observation
        .snapshot()
        .unwrap()
        .sources
        .iter()
        .find(|coverage| coverage.source == culprit_core::source_id::PERFORMANCE_COUNTERS)
        .unwrap();
    let observations = records
        .iter()
        .filter_map(|record| {
            let culprit_core::telemetry::TelemetryRecord::Operational(row) = record else { panic!() };
            (row.source == coverage.source).then_some(row)
        })
        .collect::<Vec<_>>();
    let shared = culprit_checks::performance_counters::explain_source(&source, coverage, &observations, true).unwrap();
    let selected = culprit_checks::performance_counters::unavailable(
        culprit_core::PerformanceCounterPopulation::SelectedDatabase,
        &culprit_core::message!("view.performance_counters.sharing_withheld"),
    );
    report.performance_counters = vec![shared, selected];
    let value = compact_report(&report, Path::new("saved-report.json"));
    assert_eq!(value["performance_counters"], serde_json::to_value(&report.performance_counters).unwrap());
    assert_eq!(value["performance_counters"][0]["population"], "shared-server");
    assert_eq!(value["performance_counters"][1]["population"], "selected-database");
    assert_eq!(value["performance_counters"][1]["source_state"], "unavailable");
    assert!(value["performance_counters"][1]["native_counter_rows"].is_null());
    assert!(value.get("operations").is_none(), "compact scan output does not include private original receipts");
}
