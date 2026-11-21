use super::*;
use culprit_collector::analytical::test_support::{PoolSettingsCase, pool_settings_evidence};
fn report(case: PoolSettingsCase) -> PoolSettingsReport {
    let (source, rows) = pool_settings_evidence(case);
    let rows = rows
        .iter()
        .map(|row| {
            let culprit_core::telemetry::TelemetryRecord::Setting(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::pool_settings::explain(culprit_checks::pool_settings::PoolSettingsEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn compact_pool_configuration_keeps_native_units_null_and_zero_without_raw_member_proof() {
    for case in PoolSettingsCase::ALL {
        let report = report(case);
        let value = value(&report);
        assert_eq!(value["scope"], "pool");
        assert_eq!(value["member_database"], "recorded-member");
        if case == PoolSettingsCase::SingleDatabase {
            assert_eq!(value["target_scope"], "database");
            assert!(value["pool"].is_null());
        }
        assert_eq!(value["fields"].as_array().unwrap().len(), report.fields.len());
        if case == PoolSettingsCase::Exact {
            assert_eq!(value["fields"][0]["native_value"], "0");
            assert!(value["fields"][3]["value"].is_null());
            assert_eq!(value["observed_at"], "2026-11-01T00:00:02Z");
        }
        for private in [
            "logical_database_guid",
            "physical_database_guid",
            "11111111-1111-1111-1111-111111111111",
            "manifest_sha256",
            "qualifiers",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
    }
}
#[test]
fn compact_pool_fields_text_and_limitations_are_bounded_even_for_extra_supplied_report_fields() {
    let mut report = report(PoolSettingsCase::Exact);
    report.fields.extend(report.fields.clone());
    report.fields[0].explanation = "x".repeat(5000);
    report.limitations = vec!["l".repeat(5000); 12];
    report.verify = "v".repeat(5000);
    let value = value(&report);
    assert_eq!(value["fields"].as_array().unwrap().len(), 6);
    assert_eq!(value["fields_available"], 12);
    assert_eq!(value["fields_limited"], true);
    assert_eq!(value["limitations"].as_array().unwrap().len(), 8);
    assert_eq!(value["fields"][0]["explanation"].as_str().unwrap().chars().count(), 1025);
    assert_eq!(value["verify"].as_str().unwrap().chars().count(), 1025);
}
#[test]
fn withdrawn_original_does_not_publish_a_known_zero_native_population_or_recover_owner_clocks() {
    let report =
        culprit_checks::pool_settings::unavailable(&culprit_core::message!("view.pool_settings.sharing_withheld"));
    let value = value(&report);
    assert!(value["native_rows"].is_null() && value["pool"].is_null() && value["original_read_bounds"].is_null());
    assert!(value["fields"].as_array().unwrap().is_empty());
    assert_eq!(value["source_state"], "unavailable");
}
