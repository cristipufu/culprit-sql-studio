use super::*;
use culprit_collector::analytical::test_support::{ColumnstoreRowgroupCase, columnstore_rowgroup_evidence};
use culprit_core::telemetry::TelemetryRecord;
fn report(case: ColumnstoreRowgroupCase) -> ColumnstoreRowgroupReport {
    let (source, records) = columnstore_rowgroup_evidence(case);
    let rows = records
        .iter()
        .map(|row| match row {
            TelemetryRecord::Operational(row) => row,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    culprit_checks::columnstore_rowgroups::explain(culprit_checks::columnstore_rowgroups::ColumnstoreRowgroupEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn human_rowgroup_values_keep_exact_native_metrics_qualifications_and_bounded_followup() {
    for case in ColumnstoreRowgroupCase::ALL {
        let report = report(case);
        let text = columnstore_rowgroups(&report);
        assert!(text.contains(&report.summary));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        for entry in report.entries.iter().take(crate::output::BRIEF_FINDING_LIMIT) {
            assert!(text.contains(&entry.title));
            for metric in &entry.metrics {
                assert!(text.contains(&culprit_checks::metric_text(metric)));
            }
            assert!(text.contains(&report.investigation));
            assert!(text.contains(&report.verify));
        }
        for private in ["source_sha256", "native-instance", "original.invalid", "observation_key"] {
            assert!(!text.contains(private), "{private}");
        }
    }
}
#[test]
fn rowgroup_observations_do_not_print_an_unbounded_native_roster() {
    let mut report = report(ColumnstoreRowgroupCase::States);
    let mut extra = report.entries[0].clone();
    while report.entries.len() < crate::output::BRIEF_FINDING_LIMIT {
        report.entries.push(extra.clone());
    }
    extra.title = "omitted-rowgroup-entry".into();
    report.entries.push(extra);
    assert!(!columnstore_rowgroups(&report).contains("omitted-rowgroup-entry"));
}
