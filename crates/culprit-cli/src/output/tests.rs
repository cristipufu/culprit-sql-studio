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

fn suspect_history(
    case: culprit_collector::analytical::test_support::SuspectPageHistoryCase,
) -> culprit_core::SuspectPageHistoryReport {
    use culprit_core::telemetry::*;
    let (source, records) = culprit_collector::analytical::test_support::suspect_page_history_evidence(case);
    let rows = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::suspect_pages::explain(&SuspectPageHistoryRead::from_source(&source, &rows, true).unwrap()).unwrap()
}

#[test]
fn compact_scan_history_stays_visible_without_an_event_finding_or_private_original_proof() {
    use culprit_collector::analytical::test_support::SuspectPageHistoryCase;
    for case in SuspectPageHistoryCase::ALL {
        let mut scan = report();
        scan.findings.clear();
        let history = suspect_history(case);
        let original = serde_json::to_value(&history).unwrap();
        scan.suspect_pages = Some(Box::new(history.clone()));
        let output = compact_report(&scan, Path::new("saved-report.json"));
        let shown = &output["suspect_pages"];
        assert!(output["findings"].as_array().unwrap().is_empty());
        assert_eq!(shown["active_rows"], serde_json::to_value(&history.active_rows).unwrap());
        assert_eq!(shown["retained_rows"], serde_json::to_value(&history.retained_rows).unwrap());
        assert_eq!(shown["native_database_id"], serde_json::to_value(&history.native_database_id).unwrap());
        assert_eq!(shown["observed_at"], serde_json::to_value(&history.observed_at).unwrap());
        assert_eq!(
            shown["last_reported_time_without_offset"],
            serde_json::to_value(&history.last_reported_time_without_offset).unwrap()
        );
        assert_eq!(shown["source_state"], serde_json::to_value(history.source_state).unwrap());
        assert_eq!(shown["limitations"], serde_json::to_value(&history.limitations).unwrap());
        if let Some(context) = &history.context {
            let (from, until) = context.bounds();
            assert_eq!(shown["read_bounds"]["observed_from"], from);
            assert_eq!(shown["read_bounds"]["observed_until"], until);
        }
        let keys = shown.as_object().unwrap().keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "title",
                "summary",
                "source_state",
                "source_reason",
                "read_complete",
                "database",
                "native_database_id",
                "observed_at",
                "active_rows",
                "retained_rows",
                "last_reported_time_without_offset",
                "read_bounds",
                "metrics",
                "limitations",
            ])
        );
        for private in [
            "private-member",
            "private-process",
            "private-database-guid",
            "manifest_sha256",
            "qualifiers",
            "native_source",
        ] {
            assert!(!shown.to_string().contains(private));
        }
        if matches!(case, SuspectPageHistoryCase::Denied | SuspectPageHistoryCase::Unsupported) {
            assert!(shown["active_rows"].is_null());
            assert!(shown["retained_rows"].is_null());
        }
        assert_eq!(serde_json::to_value(scan.suspect_pages.as_ref().unwrap()).unwrap(), original);
    }
}

#[test]
fn markdown_scan_preserves_historical_counts_and_limits_as_bounded_text_without_an_event_finding() {
    use culprit_collector::analytical::test_support::SuspectPageHistoryCase;
    for case in SuspectPageHistoryCase::ALL {
        let mut scan = report();
        scan.findings.clear();
        let history = suspect_history(case);
        let expected = observations::suspect_pages(&history);
        scan.suspect_pages = Some(Box::new(history.clone()));
        let output = markdown_report(&scan);
        assert!(output.contains(&expected));
        assert!(output.contains(&history.title));
        assert!(output.contains(&history.summary));
        for limitation in &history.limitations {
            assert!(output.contains(limitation));
        }
    }
    let mut scan = report();
    scan.findings.clear();
    let mut history = suspect_history(SuspectPageHistoryCase::Active);
    history.database = Some("shop\n```\n<img src=x> [link](javascript:test)\u{1b}[31m".into());
    let original = serde_json::to_value(&history).unwrap();
    scan.suspect_pages = Some(Box::new(history));
    let output = markdown_report(&scan);
    let opening = output.lines().find(|line| line.starts_with("````") && line.ends_with("text")).unwrap();
    let fence = opening.strip_suffix("text").unwrap();
    assert_eq!(output.lines().last(), Some(fence));
    assert!(output.contains("<img src=x>"), "native labels are literal content inside the enclosing text fence");
    assert!(!output.contains('\u{1b}'));
    assert_eq!(serde_json::to_value(scan.suspect_pages.as_ref().unwrap()).unwrap(), original);
}
