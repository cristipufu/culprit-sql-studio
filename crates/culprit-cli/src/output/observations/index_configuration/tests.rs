use super::*;
use culprit_collector::analytical::test_support::{IndexConfigurationCase, index_configuration_evidence};
use culprit_core::telemetry::*;
fn report(case: IndexConfigurationCase) -> IndexConfigurationReport {
    let (description, records) = index_configuration_evidence(case);
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
    culprit_checks::index_configuration::explain(culprit_checks::table_catalog::MssqlCatalogEvidence {
        description: &description,
        observations: &observations,
        source: &source,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn human_configuration_uses_original_canonical_states_and_separate_partition_values() {
    for case in IndexConfigurationCase::ALL {
        let report = report(case);
        let text = index_configuration(&report);
        assert!(text.contains(&report.title));
        assert!(text.contains(&report.summary));
        assert!(text.contains(&report.qualification));
        for entry in report.entries.iter().take(ENTRY_LIMIT) {
            for field in &entry.fields {
                assert!(text.contains(&field.text));
            }
            for partition in entry.partitions.iter().take(PARTITION_LIMIT) {
                assert!(text.contains(&partition.partition_id));
            }
        }
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
    }
}
#[test]
fn human_native_names_and_future_descriptions_are_bounded_escaped_text() {
    let mut report = report(IndexConfigurationCase::UnknownCode);
    report.entries[0].schema = "schema\n\u{1b}[31m".into();
    report.entries[0].table = "界".repeat(2000);
    report.entries[0].partitions[0].fields[1].text = "<native>\n\u{1b}[31m".into();
    let before = serde_json::to_value(&report).unwrap();
    let text = index_configuration(&report);
    assert!(text.contains("schema\\n\\u{1b}[31m"));
    assert!(text.contains("<native>\\n\\u{1b}[31m"));
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains(&report.entries[0].table));
    assert_eq!(serde_json::to_value(&report).unwrap(), before);
}

#[test]
fn compact_original_bounds_and_receipt_privacy_share_the_named_presentation_caps() {
    let mut report = report(IndexConfigurationCase::Partial);
    report.entries = vec![report.entries[0].clone(); 10];
    let before = serde_json::to_value(&report).unwrap();
    let compact = compact_index_configuration(&report);
    assert_eq!(compact["returned_rows"], ENTRY_LIMIT);
    assert_eq!(compact["entries"][0]["returned_partitions"], PARTITION_LIMIT);
    assert_eq!(compact["entries"][0]["retained_partitions"], 64);
    assert_eq!(compact["entries"][0]["partitions_complete"], false);
    for private in ["observation_key", "manifest_sha256", "source_sha256", "epoch", "namespace"] {
        assert!(!compact.to_string().contains(private), "{private}");
    }
    assert_eq!(serde_json::to_value(&report).unwrap(), before);
}
