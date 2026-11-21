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
fn native_interval_cli_keeps_qualified_wide_totals_null_unavailable_and_no_current_scope() {
    for case in ManagedInstanceResourceCase::ALL {
        let report = report(case);
        let output = managed_instance_resources(&report);
        let compact = compact(&report);
        assert!(output.contains(&report.title));
        assert_eq!(compact["scope"], "shared-instance-history");
        assert_eq!(compact["source_state"], serde_json::to_value(report.source_state).unwrap());
        if case == ManagedInstanceResourceCase::Complete {
            assert!(output.contains("9007199254740993"));
            assert!(output.contains("9007199254740992.25"));
            assert!(output.contains(&report.investigation));
            assert!(output.contains(&report.verify));
        }
        if matches!(case, ManagedInstanceResourceCase::Denied | ManagedInstanceResourceCase::Unsupported) {
            assert!(compact["native_rows"].is_null());
            assert!(compact["intervals"].as_array().unwrap().is_empty());
        }
        for secret in [
            "private-current-machine",
            "private-current-instance",
            "private-current-process",
            "manifest_sha256",
            "payload_sha256",
        ] {
            assert!(!output.contains(secret));
            assert!(!compact.to_string().contains(secret));
        }
    }
}
#[test]
fn terminal_native_labels_and_compact_intervals_are_bounded_escaped_and_leave_original_json_unchanged() {
    let mut report = report(ManagedInstanceResourceCase::Complete);
    report.intervals[0].fields[8].value = Some("line\n\u{1b}[31m🌿<script>".repeat(300));
    report.intervals.extend(report.intervals.clone());
    report.intervals.extend(report.intervals.clone());
    let original = serde_json::to_value(&report).unwrap();
    let output = managed_instance_resources(&report);
    let compact = compact(&report);
    assert_eq!(compact["intervals"].as_array().unwrap().len(), 3);
    assert!(!output.contains('\u{1b}'));
    assert!(!output.contains("line\n"));
    assert!(output.contains("\\n"));
    assert!(!output.contains('�'));
    assert_eq!(serde_json::to_value(&report).unwrap(), original);
    let unavailable = culprit_checks::managed_instance_resources::unavailable(&culprit_core::message!(
        "view.mi_resources.sharing_withheld"
    ));
    let value = super::compact(&unavailable);
    assert!(value["native_rows"].is_null());
    assert!(value["recorded_rows"].is_null());
    assert!(value["observed_at"].is_null());
}
