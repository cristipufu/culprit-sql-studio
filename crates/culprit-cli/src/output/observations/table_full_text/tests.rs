use super::*;
use culprit_collector::analytical::test_support::{TableFullTextCase, table_full_text_evidence};
use culprit_core::telemetry::*;
fn report(case: TableFullTextCase) -> culprit_core::TableFullTextReport {
    let (description, records) = table_full_text_evidence(case);
    let coverage = &description.observation.snapshot().unwrap().sources[0];
    let source = culprit_core::SourceOutcome {
        source: coverage.source.clone(),
        state: coverage.state,
        reason: None,
        reason_message: None,
    };
    let rows = records
        .iter()
        .map(|record| match record {
            TelemetryRecord::Operational(row) => row,
            _ => panic!(),
        })
        .collect::<Vec<_>>();
    culprit_checks::table_full_text::explain(culprit_checks::table_full_text::TableFullTextEvidence {
        description: &description,
        observations: &rows,
        source: &source,
        read_complete: true,
    })
    .unwrap()
}

#[test]
fn canonical_full_text_states_remain_informational_and_native_null_is_not_false() {
    for case in TableFullTextCase::ALL {
        let report = report(case);
        let output = table_full_text(&report);
        assert!(output.contains(&report.title));
        for entry in &report.entries {
            assert!(output.contains(&entry.summary));
            assert!(output.contains(&entry.qualification));
            for metric in &entry.metrics {
                assert!(output.contains(metric.text.as_deref().unwrap()));
            }
        }
        assert!(!output.contains("payload_sha256") && !output.contains("CREATE FULLTEXT"));
    }
}
#[test]
fn existing_three_entry_control_and_unicode_bounds_apply_to_table_catalog_context() {
    let mut report = report(TableFullTextCase::Enabled);
    let entry = report.entries[0].clone();
    report.entries = vec![entry; 9];
    report.entries[0].schema = "schema\n\u{1b}[31m".into();
    report.entries[0].table = "界".repeat(2048);
    let output = table_full_text(&report);
    assert_eq!(output.matches('•').count(), super::super::super::BRIEF_FINDING_LIMIT);
    assert!(
        output
            .lines()
            .filter(|line| line.starts_with("• "))
            .all(|line| line.chars().count() <= OBSERVATION_DETAIL_CHARS + 3)
    );
    assert!(!output.contains(&report.entries[0].table));
    assert!(!output.contains('\u{1b}'));
    assert!(output.contains("schema\\n\\u{1b}[31m"));
    assert_eq!(output.lines().filter(|line| line.starts_with("• ")).count(), super::super::super::BRIEF_FINDING_LIMIT);
}
