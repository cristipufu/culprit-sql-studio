use super::*;
use culprit_collector::analytical::test_support::{TableTemporalCase, table_temporal_evidence};
use culprit_core::telemetry::*;
fn report(case: TableTemporalCase) -> TableTemporalReport {
    let (description, records) = table_temporal_evidence(case);
    let coverage = &description.observation.snapshot().unwrap().sources[0];
    let source = culprit_core::SourceOutcome {
        source: coverage.source.clone(),
        state: coverage.state,
        reason: None,
        reason_message: None,
    };
    let observations = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::table_temporal::explain(culprit_checks::table_catalog::MssqlCatalogEvidence {
        description: &description,
        observations: &observations,
        source: &source,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn readable_configuration_uses_canonical_fields_availability_and_original_limits() {
    for case in TableTemporalCase::ALL {
        let report = report(case);
        let text = table_temporal(&report);
        assert!(text.contains(&report.title));
        assert!(text.contains(&report.summary));
        for entry in &report.entries {
            for field in &entry.fields {
                assert!(text.contains(&field.text));
            }
            assert!(text.contains(&report.qualification));
        }
        assert!(text.contains(&report.limitations[0]));
    }
}
#[test]
fn existing_entry_and_unicode_bounds_escape_native_labels_without_printing_original_keys() {
    let mut report = report(TableTemporalCase::SystemVersioned);
    report.entries = vec![report.entries[0].clone(); 9];
    report.entries[0].schema = "schema\n\u{1b}[31m".into();
    report.entries[0].table = "界".repeat(2048);
    let text = table_temporal(&report);
    assert_eq!(text.matches('•').count(), super::super::super::BRIEF_FINDING_LIMIT);
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("schema\\n\\u{1b}[31m"));
    assert!(!text.contains(&report.entries[0].table));
    assert!(
        text.lines()
            .filter(|line| line.starts_with("• "))
            .all(|line| line.chars().count() <= OBSERVATION_DETAIL_CHARS + 3)
    );
}
