use super::*;
use culprit_collector::analytical::test_support::{TableMemoryCase, table_memory_evidence};
use culprit_core::telemetry::TelemetryRecord;
fn report(case: TableMemoryCase) -> TableMemoryReport {
    let (source, records) = table_memory_evidence(case);
    let rows = records
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::table_memory::explain(culprit_checks::table_memory::TableMemoryEvidence {
        source: &source,
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}
#[test]
fn selected_table_memory_human_output_keeps_exact_native_metrics_and_every_original_qualification() {
    for case in TableMemoryCase::ALL {
        let report = report(case);
        let text = table_memory(&report);
        assert!(text.contains(&report.title));
        assert!(text.contains(&report.summary));
        assert!(text.contains("dbo.Hot"));
        for metric in &report.metrics {
            assert!(text.contains(&culprit_checks::metric_text(metric)));
        }
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        for private in [
            "private-machine-canary",
            "private-instance-canary",
            "private-process-and-database-canary",
            "manifest_sha256",
            "source_sha256",
            "original.invalid",
        ] {
            assert!(!text.contains(private), "{private}");
        }
    }
}
#[test]
fn native_selected_table_labels_use_existing_escaped_bounded_unicode_preview() {
    let mut report = report(TableMemoryCase::Exact);
    report.original.as_mut().unwrap().metadata.schema = "dbo\n\u{1b}[31m".into();
    report.original.as_mut().unwrap().metadata.name = format!("<table>&{}", "界".repeat(500));
    let text = table_memory(&report);
    assert!(text.contains(&statement::preview(&report.original.as_mut().unwrap().metadata.schema)));
    assert!(text.contains(&statement::preview(&report.original.as_mut().unwrap().metadata.name)));
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains(&report.original.as_mut().unwrap().metadata.schema));
    assert!(!text.contains(&report.original.as_mut().unwrap().metadata.name));
    assert!(text.len() < 4096);
}
