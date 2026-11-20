use super::*;
use culprit_collector::analytical::test_support::{SuspectPageHistoryCase, suspect_page_history_evidence};
use culprit_core::telemetry::*;
fn report(case: SuspectPageHistoryCase) -> SuspectPageHistoryReport {
    let (source, rows) = suspect_page_history_evidence(case);
    let observations = rows
        .iter()
        .map(|row| match row {
            TelemetryRecord::Operational(row) => row,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    culprit_checks::suspect_pages::explain(&SuspectPageHistoryRead::from_source(&source, &observations, true).unwrap())
        .unwrap()
}
#[test]
fn human_original_history_preserves_counts_native_local_time_and_all_historical_limits() {
    for case in SuspectPageHistoryCase::ALL {
        let report = report(case);
        let text = suspect_pages(&report);
        assert!(text.contains(&report.summary));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        for metric in &report.metrics {
            assert!(text.contains(metric.text.as_deref().unwrap()));
        }
        for private in ["private-member", "private-process", "private-database-guid", "manifest_sha256", "qualifiers"] {
            assert!(!text.contains(private));
        }
    }
}
#[test]
fn native_database_labels_are_escaped_without_changing_the_original_report() {
    let mut report = report(SuspectPageHistoryCase::Active);
    report.database = Some("shop\n\u{1b}[31m🧪".into());
    let before = serde_json::to_value(&report).unwrap();
    let text = suspect_pages(&report);
    assert!(!text.contains("shop\n"));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("🧪"));
    assert_eq!(serde_json::to_value(&report).unwrap(), before);
}

#[test]
fn point_utc_and_last_active_native_local_time_keep_separate_labels_and_values() {
    let report = report(SuspectPageHistoryCase::Active);
    let point = report.observed_at.as_deref().unwrap();
    let local = report.last_reported_time_without_offset.as_deref().unwrap();
    let (from, until) = report.context.as_ref().unwrap().bounds();
    assert_ne!(point, from);
    assert_ne!(point, until);
    let before = serde_json::to_value(&report).unwrap();
    let text = suspect_pages(&report);
    assert!(text.contains(&t!(
        "view.counter_ratios.cli.metric",
        label = t!("view.database_properties.native_clock"),
        value = point
    )));
    assert!(text.contains(&t!(
        "view.counter_ratios.cli.metric",
        label = t!("view.suspect_pages.last_active_time"),
        value = local
    )));
    assert!(!text.contains(&format!("{local}Z")), "the native local maximum has no established UTC offset");
    assert_eq!(serde_json::to_value(&report).unwrap(), before);
}
