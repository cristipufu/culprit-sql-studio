use super::*;
use culprit_collector::analytical::test_support::{FullTextPopulationCase, full_text_population_evidence};
use culprit_core::telemetry::TelemetryRecord;
fn report(case: FullTextPopulationCase) -> FullTextPopulationReport {
    let (source, records) = full_text_population_evidence(case);
    let rows = records
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::full_text_populations::explain(culprit_checks::full_text_populations::FullTextPopulationEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn original_runtime_summary_keeps_native_state_local_time_qualifications_and_no_private_source_proof() {
    for case in FullTextPopulationCase::ALL {
        let report = report(case);
        let text = full_text_populations(&report);
        assert!(text.contains(&report.title));
        assert!(text.contains(&report.summary));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        for entry in &report.entries {
            for metric in &entry.metrics {
                assert!(text.contains(&culprit_checks::metric_text(metric)));
            }
        }
        for private in [
            "native-machine",
            "native-member",
            "original-process-and-db",
            "manifest_sha256",
            "source_sha256",
            "original.invalid",
        ] {
            assert!(!text.contains(private), "{private}");
        }
    }
}
#[test]
fn unavailable_and_empty_outputs_remain_distinct_and_do_not_need_a_current_table_lookup() {
    let denied = full_text_populations(&report(FullTextPopulationCase::Denied));
    let empty = full_text_populations(&report(FullTextPopulationCase::Empty));
    assert_ne!(denied, empty);
    assert!(empty.contains(&culprit_core::t!("view.full_text_populations.empty")));
    let native = full_text_populations(&report(FullTextPopulationCase::Processing));
    assert!(native.contains("2019-12-31T18:00:00.007"));
    assert!(!native.contains("2019-12-31T18:00:00.007Z"));
}
#[test]
fn actual_original_table_labels_use_the_shared_escaped_unicode_preview_without_altering_the_report() {
    let mut report = report(FullTextPopulationCase::Processing);
    let original = report.original.as_mut().unwrap();
    original.metadata.schema = Some("dbo\n\u{1b}[31m".into());
    original.metadata.table = Some(format!("<table>&{}", "界".repeat(2000)));
    let before = serde_json::to_value(&report).unwrap();
    let text = full_text_populations(&report);
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains("dbo\n"));
    assert!(text.chars().filter(|ch| *ch == '界').count() <= 1024);
    assert!(text.contains(&crate::output::observations::statement::preview(
        report.original.as_ref().unwrap().metadata.schema.as_deref().unwrap()
    )));
    assert_eq!(serde_json::to_value(&report).unwrap(), before);
}
