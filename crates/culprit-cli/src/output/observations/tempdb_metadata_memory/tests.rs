use super::*;
use culprit_checks::tempdb_metadata_memory::{TempdbMetadataMemoryEvidence, explain};
use culprit_collector::analytical::test_support::{TempdbMetadataMemoryCase, tempdb_metadata_memory_evidence};
use culprit_core::{SourceState, telemetry::*};

fn report(case: TempdbMetadataMemoryCase) -> TempdbMetadataMemoryReport {
    let (source, records) = tempdb_metadata_memory_evidence(case);
    let rows = records
        .iter()
        .filter_map(|record| match record {
            TelemetryRecord::Operational(row) if row.source == culprit_core::source_id::TEMPDB_METADATA_MEMORY => {
                Some(row)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    explain(TempdbMetadataMemoryEvidence {
        context: &NativeReadContext::from_snapshot(&source).unwrap(),
        coverage: &source.observation.snapshot().unwrap().sources[0],
        observations: &rows,
        read_complete: true,
    })
    .unwrap()
}

#[test]
fn human_metadata_output_preserves_exact_bytes_original_clocks_actions_and_every_canonical_qualification() {
    for case in TempdbMetadataMemoryCase::ALL {
        let report = report(case);
        let original = report.clone();
        let text = tempdb_metadata_memory(&report);
        assert!(text.contains(&report.summary));
        let (from, until) = report.context.bounds();
        assert!(text.contains(from) && text.contains(until));
        assert!(text.contains(&t!("view.tempdb_metadata_memory.field.feature_observed_at")));
        assert!(text.contains(report.tempdb_created_without_offset.as_deref().unwrap()));
        for entry in &report.entries {
            assert!(text.contains(&format::digits(entry.allocated_bytes.clone().unwrap())));
            assert!(text.contains(&format::digits(entry.used_bytes.clone().unwrap())));
            for words in [&entry.explanation, &entry.investigation, &entry.verify] {
                assert!(text.contains(words));
            }
        }
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        if report.source_state == SourceState::Partial {
            assert!(text.contains(&t!("evidence.cli.source_partial")));
        }
        if case == TempdbMetadataMemoryCase::Unknown {
            assert!(text.contains(&t!("view.wait_counters.unknown")));
        }
        if report.entries.is_empty() {
            assert!(!text.contains(&t!("view.tempdb_metadata_memory.field.reserved")));
        }
        for private in [
            "native-instance-a",
            "native-instance-namespace",
            "observation_key",
            "qualifiers",
            "manifest_sha256",
            "source_sha256",
        ] {
            assert!(!text.contains(private), "{private}");
        }
        assert_eq!(report, original);
    }
}

#[test]
fn human_metadata_output_limits_three_entries_and_each_unicode_detail_without_printing_private_references() {
    let mut report = report(TempdbMetadataMemoryCase::Reserved);
    let mut template = report.entries.remove(0);
    let long = "界é".repeat(super::super::OBSERVATION_DETAIL_CHARS);
    template.explanation = long.clone();
    template.investigation = long.clone();
    template.verify = long.clone();
    report.entries = (0..6)
        .map(|index| {
            let mut entry = template.clone();
            entry.description = Some(format!("consumer-{index}"));
            entry.observation_key = format!("private-row-key-{index}");
            entry
        })
        .collect();
    let text = tempdb_metadata_memory(&report);
    for index in 0..super::super::super::BRIEF_FINDING_LIMIT {
        assert!(text.contains(&format!("consumer-{index}")));
    }
    assert!(!text.contains("consumer-3"));
    assert!(text.contains(&t!("view.tempdb_metadata_memory.limit.presentation")));
    assert!(!text.contains(&long));
    let bound = super::super::OBSERVATION_DETAIL_CHARS;
    assert!(text.lines().filter(|line| line.starts_with('界')).all(|line| line.chars().count() <= bound + 1));
    assert!(!text.contains("private-row-key"));
    report.entries[0].used_bytes = None;
    let text = tempdb_metadata_memory(&report);
    assert!(text.contains(&t!(
        "view.counter_ratios.cli.metric",
        label = t!("view.tempdb_metadata_memory.field.used"),
        value = t!("view.wait_counters.unknown")
    )));
}
