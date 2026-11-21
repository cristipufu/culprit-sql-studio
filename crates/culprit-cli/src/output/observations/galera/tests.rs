use super::*;
use culprit_collector::analytical::test_support::{GaleraCase, galera_evidence};
fn report(case: GaleraCase) -> GaleraReport {
    let (source, records) = galera_evidence(case);
    let rows = records
        .iter()
        .map(|r| {
            let culprit_core::telemetry::TelemetryRecord::Operational(row) = r else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::galera::explain(culprit_checks::galera::GaleraEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn canonical_member_state_exact_wide_totals_queues_and_next_steps_are_bounded_without_native_proof_payloads() {
    let report = report(GaleraCase::Queued);
    let text = galera(&report);
    assert!(text.contains(&report.title));
    assert!(text.contains(&report.entries[0].explanation));
    assert!(text.contains(&culprit_core::format::digits("9007199254740993".into())));
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(text.contains("2020-01-01T00:00:00Z"));
    assert!(text.chars().count() < 8192);
    let value = compact(&report);
    assert_eq!(value["native_rows"], 1);
    assert_eq!(
        value["entries"][0]["groups"][1]["fields"][0]["value"],
        culprit_core::format::digits("9007199254740993".into())
    );
    for private in [
        "aaaaaaaa-0000-4000-8000-000000000001",
        "bbbbbbbb-0000-4000-8000-000000000002",
        "private-galera-process-canary",
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
        GaleraCase::Zero,
        GaleraCase::NativeNulls,
        GaleraCase::Denied,
        GaleraCase::MissingVariable,
        GaleraCase::Partial,
        GaleraCase::Disabled,
    ] {
        let report = report(case);
        let value = compact(&report);
        assert_eq!(value["sources"], serde_json::to_value(&report.sources).unwrap());
        assert_eq!(value["read_complete"], true);
        assert_eq!(value["native_rows"], report.native_rows.unwrap());
        if case == GaleraCase::Zero {
            assert_eq!(value["entries"][0]["groups"][0]["fields"][0]["value"], "0");
        }
        if case == GaleraCase::NativeNulls {
            assert!(value["entries"][0]["groups"][0]["fields"][0]["value"].is_null());
        }
        if case == GaleraCase::Disabled {
            assert!(value["entries"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn native_component_text_is_escaped_without_mutating_the_original_derived_report() {
    let mut report = report(GaleraCase::Queued);
    let field = report.entries[0].fields.iter_mut().find(|field| field.key == "wsrep_cluster_status").unwrap();
    field.value = Some("component\n\u{1b}[31m🌿".into());
    let original = serde_json::to_value(&report).unwrap();
    let text = galera(&report);
    assert!(!text.contains("component\n"));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("component\\n"));
    assert_eq!(serde_json::to_value(&report).unwrap(), original);
}
