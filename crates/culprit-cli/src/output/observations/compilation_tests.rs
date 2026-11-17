use super::*;
use culprit_checks::compilation::{CompilationEvidence, QueryStoreDefinitionEvidence, QueryStoreDefinitionRecord};
use culprit_collector::analytical::test_support::{Recovery, compilation_definition_evidence_cases};
use culprit_core::telemetry::*;

fn report_for(name: &str, missing_definition: bool) -> CompilationReport {
    let case = compilation_definition_evidence_cases().into_iter().find(|case| case.name == name).unwrap();
    let context = NativeReadContext::from_snapshot(&case.current.0).unwrap();
    let SourceObservation::Snapshot(snapshot) = &case.current.0.observation else { panic!() };
    let coverage = &snapshot.sources[0];
    let outcome =
        SourceOutcome { source: coverage.source.clone(), state: coverage.state, reason: None, reason_message: None };
    let observations = case
        .current
        .1
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    let (source, records) = case.definitions.unwrap();
    let recovery =
        Recovery::capture("unit:compiler-renderer", &digest(b"schema"), source.clone(), records.clone(), 970_000);
    let definitions = if missing_definition {
        vec![]
    } else {
        records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let TelemetryRecord::QueryDefinition(definition) = record else { panic!() };
                let row = ExportedRecord::new(&source, &recovery.manifest.capture, record.clone()).unwrap();
                QueryStoreDefinitionRecord {
                    reference: DiagnosticEvidenceRef::from_export(
                        &source,
                        &recovery.manifest.capture_manifest_sha256,
                        &row,
                        index as u32,
                    )
                    .unwrap(),
                    definition,
                }
            })
            .collect()
    };
    culprit_checks::compilation::explain(CompilationEvidence {
        context: Some(&context),
        observations: &observations,
        source: Some(&outcome),
        definitions: Some(QueryStoreDefinitionEvidence {
            source: &source,
            rows: &definitions,
            selected_rows: records.len() as u32,
            read_complete: !missing_definition,
        }),
    })
    .unwrap()
}

#[test]
fn compiler_text_and_json_keep_measured_footprints_qualified_statement_and_exact_counts() {
    let report = report_for("matched", false);
    let text = compilation(&report);
    assert!(text.contains("original_compile_input"));
    assert!(!text.contains("81724"));
    assert!(text.contains(&t!("common.metric.compile_memory_last")));
    for metric in &report.entries[0].metrics {
        if let Some(value) = &metric.text {
            assert!(text.contains(&format!("{}: {value}", metric.label)), "missing metric {}", metric.key);
        }
    }
    for qualification in &report.limitations {
        assert!(text.contains(qualification), "missing {qualification}: {text}");
    }
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["entries"][0]["statement"]["binding"], "matched");
    assert_eq!(json["entries"][0]["statement"]["origin"], "application");
    assert!(json["entries"][0].get("current_compiler").is_none());
    let large = report_for("native_large_count", false);
    assert!(compilation(&large).contains("9007199254740993"));
    assert!(large.entries[0].metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993")));
}

#[test]
fn a_long_native_field_does_not_hide_later_facts_or_expand_the_observation_limit() {
    let mut report = report_for("matched", false);
    let original = report.entries[0].clone();
    report.entries = (0..super::super::BRIEF_FINDING_LIMIT + 1)
        .map(|index| {
            let mut entry = original.clone();
            entry.title = format!("bounded-native-observation-{index}");
            entry.explanation = "界".repeat(OBSERVATION_DETAIL_CHARS + 1);
            entry
        })
        .collect();
    let text = compilation(&report);
    assert!(text.contains(&format!("{}…", "界".repeat(OBSERVATION_DETAIL_CHARS))));
    assert!(!text.contains(&"界".repeat(OBSERVATION_DETAIL_CHARS + 1)));
    assert!(text.contains(&t!("common.metric.compile_memory_last")));
    assert!(text.contains(&original.verify));
    assert!(text.contains("bounded-native-observation-2"));
    assert!(!text.contains("bounded-native-observation-3"));
}

#[test]
fn missing_definition_authority_and_archive_rows_never_hide_their_qualifications_or_show_borrowed_sql() {
    for (case, missing) in [("missing_namespace", false), ("matched", true)] {
        let report = report_for(case, missing);
        let text = compilation(&report);
        assert!(!text.contains("original_compile_input"));
        assert!(report.entries[0].statement.as_ref().unwrap().preview.is_none());
        for qualification in &report.limitations {
            assert!(text.contains(qualification), "{case}: missing {qualification}: {text}");
        }
        assert!(text.contains(&t!(
            "evidence.cli.compilation.definition_counts",
            selected = "1",
            observed = if missing { "0" } else { "1" }
        )));
        if missing {
            assert!(!report.definition_read.as_ref().unwrap().read_complete);
            assert!(text.contains(&t!("view.compilation.statement.limit.partial")));
        }
    }
}
