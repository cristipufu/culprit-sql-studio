use super::*;
use culprit_collector::analytical::test_support::{CdcCase, cdc_evidence};
use culprit_core::telemetry::*;
fn report(case: CdcCase) -> CdcReport {
    let (source, rows) = cdc_evidence(case);
    let rows = rows
        .iter()
        .map(|row| match row {
            TelemetryRecord::Operational(row) => row,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    culprit_checks::cdc::explain(culprit_checks::cdc::CdcEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn cdc_human_output_preserves_original_actions_qualifications_and_unavailable_states() {
    for case in CdcCase::ALL {
        let report = report(case);
        let text = cdc(&report);
        assert!(text.contains(&report.summary));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        for entry in report.entries.iter().take(3) {
            assert!(text.contains(&entry.title));
            assert!(text.contains(&entry.investigation));
            assert!(text.contains(&entry.verify));
        }
        for private in
            ["00000016:00000058:0001", "original-instance", "original.invalid", "observation_key", "source_sha256"]
        {
            assert!(!text.contains(private), "{private}");
        }
    }
}
#[test]
fn cdc_human_native_values_are_escaped_and_entry_display_remains_bounded() {
    let mut report = report(CdcCase::ErrorAndProgress);
    let mut entry = report.entries[0].clone();
    while report.entries.len() < crate::output::BRIEF_FINDING_LIMIT {
        report.entries.push(entry.clone());
    }
    entry.title = "omitted-native-entry".into();
    report.entries.push(entry);
    report.entries[1].fields[0].value = Some(format!("native\n\u{1b}[31m{}", "界".repeat(200)));
    let text = cdc(&report);
    assert!(!text.contains("omitted-native-entry"));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("\\n") && text.contains("\\u{1b}"));
}
