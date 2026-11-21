use super::*;
use culprit_collector::analytical::test_support::{IndexedViewsCase, indexed_views_evidence};
use culprit_core::telemetry::*;
fn report(case: IndexedViewsCase) -> IndexedViewsReport {
    let (description, records) = indexed_views_evidence(case);
    let coverage = &description.observation.snapshot().unwrap().sources[0];
    let source = culprit_core::SourceOutcome {
        source: coverage.source.clone(),
        state: coverage.state,
        reason: None,
        reason_message: None,
    };
    let rows = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::indexed_views::explain(culprit_checks::table_catalog::MssqlCatalogEvidence {
        description: &description,
        observations: &rows,
        source: &source,
        read_complete: true,
    })
    .unwrap()
}

#[test]
fn human_output_keeps_canonical_scope_qualification_and_native_usage_absence_separate() {
    for case in IndexedViewsCase::ALL {
        let report = report(case);
        let text = indexed_views(&report);
        assert!(text.contains(&report.summary));
        assert!(text.contains(&report.qualification));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        if case == IndexedViewsCase::Present {
            assert!(text.contains("Revenue"));
            assert!(text.contains(&report.entries[0].configuration.as_ref().unwrap().partitions[0].partition_id));
        }
        if case == IndexedViewsCase::UsageNotCollected {
            assert!(text.contains(&t!("view.indexed_views.usage", state = "not-collected")));
        }
        if case == IndexedViewsCase::Denied {
            assert!(report.entries.is_empty());
            assert!(text.contains(&t!("view.indexed_views.unavailable")));
        }
    }
}
#[test]
fn native_labels_are_escaped_and_compact_limits_do_not_change_original_evidence() {
    let mut report = report(IndexedViewsCase::Present);
    report.entries[0].parent_schema = "<native>\n\u{1b}[31m".into();
    report.entries[0].parent_name = "界".repeat(2000);
    report.entries = vec![report.entries[0].clone(); 10];
    let original = serde_json::to_value(&report).unwrap();
    let text = indexed_views(&report);
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("<native>\\n\\u{1b}[31m"));
    assert!(!text.contains(&report.entries[0].parent_name));
    let compact = compact_indexed_views(&report);
    assert_eq!(compact["returned_entries"], ENTRY_LIMIT);
    assert_eq!(compact["entries_limited"], true);
    for private in ["observation_key", "manifest_sha256", "source_sha256", "namespace", "epoch"] {
        assert!(!compact.to_string().contains(private), "{private}");
    }
    assert_eq!(serde_json::to_value(&report).unwrap(), original);
}
