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
fn bounded_projection_keeps_original_native_counts_scope_usage_and_partial_absence() {
    for case in IndexedViewsCase::ALL {
        let report = report(case);
        let output = value(&report);
        assert_eq!(output["historical_context"], true);
        assert_eq!(output["native_index_rows"], json!(report.native_index_rows));
        assert_eq!(output["source"], json!(report.source));
        assert_eq!(output["limitations"], json!(report.limitations));
        assert_eq!(output["qualification"], report.qualification);
        if case == IndexedViewsCase::Present {
            assert_eq!(output["returned_entries"], 2);
            assert_eq!(output["entries"][0]["configuration"]["parent_kind"], "indexed-view");
        }
        for private in ["observation_key", "manifest_sha256", "source_sha256", "namespace", "epoch"] {
            assert!(!output.to_string().contains(private), "{private}");
        }
    }
}
#[test]
fn native_presentation_caps_preserve_the_raw_original_and_never_supply_current_scope() {
    let mut report = report(IndexedViewsCase::Present);
    report.entries[0].parent_name = "界".repeat(2000);
    report.entries = vec![report.entries[0].clone(); 10];
    let original = json!(&report);
    let output = value(&report);
    assert_eq!(output["returned_entries"], ENTRY_LIMIT);
    assert_eq!(output["entries_limited"], true);
    assert!(output["entries"][0]["parent_name"].as_str().unwrap().chars().count() <= 256);
    assert_eq!(json!(report), original);
}
