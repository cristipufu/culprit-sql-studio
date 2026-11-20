use super::*;
use culprit_collector::analytical::test_support::{BackupHistoryCase, backup_history_evidence};
use culprit_core::telemetry::TelemetryRecord;
fn report(case: BackupHistoryCase) -> BackupHistoryReport {
    let (source, records) = backup_history_evidence(case);
    let rows = records
        .iter()
        .map(|row| match row {
            TelemetryRecord::Operational(row) => row,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    culprit_checks::backup_history::explain(culprit_checks::backup_history::BackupHistoryEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn human_backup_text_reuses_the_canonical_retention_clocks_and_unavailable_qualifications() {
    for case in BackupHistoryCase::ALL {
        let report = report(case);
        let text = backup_history(&report);
        assert!(text.contains(&report.summary));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        for entry in report.entries.iter().take(crate::output::BRIEF_FINDING_LIMIT) {
            assert!(text.contains(&entry.title));
            assert!(text.contains(&entry.explanation));
            assert!(text.contains(&entry.investigation));
            assert!(text.contains(&entry.verify));
        }
        for private in ["source_sha256", "database_guid", "original.invalid", "native-instance"] {
            assert!(!text.contains(private), "{private}");
        }
    }
}
#[test]
fn backup_entry_bound_retains_finite_whole_report_caveats() {
    let mut report = report(BackupHistoryCase::MsdbNative);
    let mut extra = report.entries[0].clone();
    while report.entries.len() < crate::output::BRIEF_FINDING_LIMIT {
        report.entries.push(extra.clone());
    }
    extra.title = "omitted-backup-entry".into();
    report.entries.push(extra);
    let text = backup_history(&report);
    assert!(!text.contains("omitted-backup-entry"));
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
}
