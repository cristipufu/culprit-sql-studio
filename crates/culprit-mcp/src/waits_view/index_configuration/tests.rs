use super::*;
use culprit_collector::analytical::test_support::{IndexConfigurationCase, index_configuration_evidence};
use culprit_core::telemetry::*;
fn report() -> IndexConfigurationReport {
    let (description, records) = index_configuration_evidence(IndexConfigurationCase::Partial);
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
fn compact_original_keeps_partition_scope_and_partiality_without_native_owner_or_receipt() {
    let report = report();
    let output = value(&report);
    assert_eq!(output["historical_context"], true);
    assert_eq!(output["source_state"], "partial");
    assert_eq!(output["entries"][0]["retained_partitions"], 64);
    assert_eq!(output["entries"][0]["returned_partitions"], PARTITION_LIMIT);
    assert_eq!(output["entries"][0]["partitions_complete"], false);
    assert_eq!(output["entries"][0]["partitions_limited"], true);
    assert_eq!(output["entries"][0]["partitions"][0]["partition_id"], report.entries[0].partitions[0].partition_id);
    assert_eq!(output["limitations"], json!(report.limitations));
    for private in ["observation_key", "manifest_sha256", "source_sha256", "namespace", "epoch", "login_time"] {
        assert!(!output.to_string().contains(private), "{private}");
    }
}
#[test]
fn compact_presentation_caps_do_not_mutate_original_or_infer_whole_index_state() {
    let mut report = report();
    report.entries = vec![report.entries[0].clone(); 10];
    report.entries[0].parent_name = "界".repeat(2000);
    let before = json!(&report);
    let output = value(&report);
    assert_eq!(output["returned_rows"], ENTRY_LIMIT);
    assert_eq!(output["entries_limited"], true);
    assert!(output["entries"][0]["parent_name"].as_str().unwrap().chars().count() <= 256);
    assert_eq!(json!(report), before);
}
