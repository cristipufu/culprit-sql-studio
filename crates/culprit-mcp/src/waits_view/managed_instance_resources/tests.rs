use super::*;
use culprit_collector::analytical::test_support::{ManagedInstanceResourceCase, managed_instance_resource_case};
fn report(case: ManagedInstanceResourceCase) -> ManagedInstanceResourceReport {
    let (source, rows) = managed_instance_resource_case(case);
    let rows = rows
        .into_iter()
        .map(|row| match row {
            culprit_core::telemetry::TelemetryRecord::ResourceSample(row) => row,
            _ => panic!(),
        })
        .collect::<Vec<_>>();
    culprit_checks::managed_instance_resources::explain(
        culprit_checks::managed_instance_resources::ManagedInstanceResourceEvidence {
            source: &source,
            observations: &rows,
            read_complete: true,
        },
    )
    .unwrap()
}
#[test]
fn compact_mi_history_preserves_native_interval_totals_nullable_values_and_independent_coverage() {
    for case in ManagedInstanceResourceCase::ALL {
        let report = report(case);
        let value = value(&report);
        assert_eq!(value["scope"], "shared-instance-history");
        assert_eq!(value["source_state"], serde_json::to_value(report.source_state).unwrap());
        assert_eq!(value["from"], "2020-01-01T06:00:00Z");
        assert_eq!(value["observed_at"], "2020-01-01T06:10:00Z");
        for private in [
            "current_reader",
            "private-current-machine",
            "private-current-instance",
            "private-current-process",
            "manifest_sha256",
            "payload_sha256",
            "positions",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
        if case == ManagedInstanceResourceCase::Complete {
            assert_eq!(value["intervals"][0]["fields"][4]["native_value"], "9007199254740993");
            assert_eq!(value["intervals"][0]["fields"][5]["native_value"], "9223372036854775807");
        }
    }
}
#[test]
fn compact_history_bounds_unicode_and_whole_withdrawal_without_known_zero_or_a_stale_value() {
    let mut report = report(ManagedInstanceResourceCase::Complete);
    report.intervals[0].fields[8].value = Some("🌿".repeat(3000));
    report.intervals.extend(report.intervals.clone());
    report.intervals.extend(report.intervals.clone());
    let value = value(&report);
    assert_eq!(value["intervals"].as_array().unwrap().len(), 3);
    assert_eq!(value["intervals"][0]["fields"][8]["value"].as_str().unwrap().chars().count(), 1025);
    assert!(!value.to_string().contains('�'));
    report.source_state = SourceState::Unavailable;
    assert!(super::value(&report)["intervals"].as_array().unwrap().is_empty());
    let report = culprit_checks::managed_instance_resources::unavailable(&culprit_core::message!(
        "view.mi_resources.sharing_withheld"
    ));
    let value = super::value(&report);
    assert!(value["native_rows"].is_null());
    assert!(value["recorded_rows"].is_null());
    assert!(value["observed_at"].is_null());
    assert!(value["from"].is_null());
}
