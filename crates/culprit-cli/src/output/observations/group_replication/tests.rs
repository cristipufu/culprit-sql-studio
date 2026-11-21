use super::*;
use culprit_collector::analytical::test_support::{GroupReplicationCase, group_replication_evidence};
fn report(case: GroupReplicationCase) -> GroupReplicationReport {
    let (source, records) = group_replication_evidence(case);
    let rows = records
        .iter()
        .map(|r| {
            let culprit_core::telemetry::TelemetryRecord::Operational(row) = r else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::group_replication::explain(culprit_checks::group_replication::GroupReplicationEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn canonical_member_state_exact_wide_totals_queues_and_next_steps_are_bounded_without_native_proof_payloads() {
    let report = report(GroupReplicationCase::Queued);
    let text = group_replication(&report);
    assert!(text.contains(&report.title));
    assert!(text.contains(&report.entries[0].explanation));
    assert!(text.contains(&culprit_core::format::digits("9007199254740993".into())));
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(text.contains("2020-01-01T00:00:00Z"));
    assert!(text.chars().count() < 8192);
    let value = compact(&report);
    assert_eq!(value["native_rows"], 2);
    assert_eq!(
        value["entries"][0]["groups"][1]["fields"][0]["value"],
        culprit_core::format::digits("9007199254740993".into())
    );
    for private in [
        "aaaaaaaa-0000-4000-8000-000000000001",
        "bbbbbbbb-0000-4000-8000-000000000002",
        "private-observer-process-canary",
        "manifest_sha256",
        "qualifiers",
        "view-original",
    ] {
        assert!(!text.contains(private));
        assert!(!value.to_string().contains(private));
    }
}
#[test]
fn independent_denied_partial_native_null_and_zero_remain_distinct_in_canonical_cli_projection() {
    for case in [
        GroupReplicationCase::OnlineZero,
        GroupReplicationCase::NativeNulls,
        GroupReplicationCase::StatisticsDenied,
        GroupReplicationCase::MembersDenied,
        GroupReplicationCase::Partial,
        GroupReplicationCase::Disabled,
    ] {
        let report = report(case);
        let value = compact(&report);
        assert_eq!(value["sources"], serde_json::to_value(&report.sources).unwrap());
        assert_eq!(value["read_complete"], true);
        assert_eq!(value["native_rows"], report.native_rows.unwrap());
        if case == GroupReplicationCase::OnlineZero {
            assert_eq!(value["entries"][0]["groups"][0]["fields"][0]["value"], "0");
        }
        if case == GroupReplicationCase::NativeNulls {
            assert!(value["entries"][0]["groups"][0]["fields"][0]["value"].is_null());
        }
        if case == GroupReplicationCase::Disabled {
            assert!(value["entries"].as_array().unwrap().is_empty());
        }
    }
}
