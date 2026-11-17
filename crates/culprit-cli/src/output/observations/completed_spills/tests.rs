use super::*;
use culprit_checks::completed_spills::{CompletedSpillEvidence, CompletedSpillRecord};
use culprit_collector::analytical::test_support::{Recovery, completed_spill_evidence_cases};
use culprit_core::telemetry::*;

fn report() -> CompletedSpillReport {
    report_for("recorded_spills")
}

fn report_for(name: &str) -> CompletedSpillReport {
    let case = completed_spill_evidence_cases().into_iter().find(|case| case.name == name).unwrap();
    let (definition_source, definition_records) = case.definitions.unwrap();
    let runtime_rows = receipts(&case.runtime.0, &case.runtime.1, 1);
    let definition_rows = receipts(&definition_source, &definition_records, 2);
    let runtime_sources = outcomes(&case.runtime.0);
    let definition_sources = outcomes(&definition_source);
    culprit_checks::completed_spills::explain(
        CompletedSpillEvidence {
            source: &case.runtime.0,
            rows: &runtime_rows,
            sources: &runtime_sources,
            read_complete: true,
        },
        Some(CompletedSpillEvidence {
            source: &definition_source,
            rows: &definition_rows,
            sources: &definition_sources,
            read_complete: true,
        }),
    )
    .unwrap()
}
fn receipts<'a>(
    source: &CollectionDescription,
    records: &'a [TelemetryRecord],
    sequence: u64,
) -> Vec<CompletedSpillRecord<'a>> {
    let recovery =
        Recovery::capture("unit:spill-renderer", &digest(b"schema"), source.clone(), records.to_vec(), sequence);
    records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let row = ExportedRecord::new(source, &recovery.manifest.capture, record.clone()).unwrap();
            CompletedSpillRecord {
                reference: DiagnosticEvidenceRef::from_export(
                    source,
                    &recovery.manifest.capture_manifest_sha256,
                    &row,
                    index as u32,
                )
                .unwrap(),
                record,
            }
        })
        .collect()
}
fn outcomes(source: &CollectionDescription) -> Vec<SourceOutcome> {
    source
        .observation
        .snapshot()
        .unwrap()
        .sources
        .iter()
        .map(|source| SourceOutcome {
            source: source.source.clone(),
            state: source.state,
            reason: None,
            reason_message: None,
        })
        .collect()
}

#[test]
fn human_and_json_keep_exact_native_pages_both_read_clocks_and_qualified_sql() {
    let report = report();
    let text = completed_spills(&report);
    for value in ["9007199254740993", "2020-01-01T12:00:00Z", "2020-01-01T12:00:03Z"] {
        assert!(text.contains(value), "{text}");
    }
    assert!(text.contains("original_spill_input"));
    assert!(!text.contains("9264"));
    for limitation in &report.limitations {
        assert!(text.contains(limitation), "missing {limitation}: {text}");
    }
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["entries"][0]["statement_binding"], "matched");
    assert!(
        json["entries"][0]["metrics"].as_array().unwrap().iter().any(|metric| metric["text"] == "9007199254740993")
    );
}

#[test]
fn human_and_json_retain_unknown_role_and_partial_source_qualifications_after_other_limits() {
    let report = report_for("partial");
    assert!(report.context.database_writable.is_none());
    assert!(report.definition_context.as_ref().unwrap().database_writable.is_none());
    assert_eq!(report.sources[0].state, SourceState::Partial);
    let text = completed_spills(&report);
    let json = serde_json::to_value(&report).unwrap();
    let qualifications = json["limitations"].as_array().unwrap();
    for key in ["view.completed_spills.limit.role", "view.completed_spills.limit.partial"] {
        let limitation = culprit_core::i18n::t(key, &[]);
        assert!(report.limitations.contains(&limitation));
        assert!(text.contains(&limitation), "missing {limitation}: {text}");
        assert!(qualifications.iter().any(|value| value.as_str() == Some(limitation.as_str())));
    }
    assert!(text.contains("9007199254740993"));
}

#[test]
fn absent_sql_and_partial_reads_remain_explicit_without_counter_invention() {
    let mut report = report();
    report.entries[0].statement_preview = None;
    report.entries[0].statement_unavailable = Some(PayloadUnavailable::Restricted);
    let text = completed_spills(&report);
    assert!(text.contains(&t!("evidence.cli.completed_spills.payload_unavailable")));
    report.entries.clear();
    report.observed_rows = 0;
    report.read_complete = false;
    report.sources[0].state = SourceState::Partial;
    report.summary = t!("view.completed_spills.summary.missing");
    let text = completed_spills(&report);
    assert!(text.contains(&report.summary));
    assert!(!text.contains("9007199254740993"));
}
