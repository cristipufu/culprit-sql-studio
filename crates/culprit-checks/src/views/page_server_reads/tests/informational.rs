use super::*;

fn unresolved(evidence: &mut Evidence, index: usize) {
    evidence.records[index].key.references = StatementReferences::default();
    let row = ExportedRecord::new(
        &evidence.source,
        &evidence.read.capture,
        TelemetryRecord::QueryRuntime(evidence.records[index].clone()),
    )
    .unwrap();
    evidence.references[index] =
        CapturedRecordReference::from_export(&evidence.read, &evidence.source, &row, index as u32).unwrap();
    evidence.reference_coverage = ReferenceCoverage {
        queries: DefinitionCoverage { required: 1, resolved: 0, unresolved: 1 },
        integrity_complete: true,
        complete: false,
        ..Default::default()
    };
}

fn explain(evidence: &Evidence, rows: &[RuntimeEvidence<'_>], read_complete: bool) -> Result<PageServerReadReport> {
    explain_archive(PageServerReadEvidence {
        source: &evidence.source,
        read: &evidence.read,
        reference_coverage: &evidence.reference_coverage,
        rows,
        selected_rows: evidence.read.captured_rows(TelemetryStream::QueryRuntime).unwrap(),
        read_complete,
        definitions: None,
    })
}

#[test]
fn an_unresolved_native_identity_keeps_statistics_but_never_becomes_a_diagnostic_reference() {
    let mut evidence = Evidence::new(1);
    unresolved(&mut evidence, 0);
    let report = explain(&evidence, &evidence.rows(), true).unwrap();
    assert!(report.read_complete);
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.observed_rows, 1);
    assert_eq!(report.entries[0].executions, "9007199254740993");
    let PageServerReadEntryIdentity::RuntimeFragment { reference, .. } = &report.entries[0].identity else { panic!() };
    assert!(reference.is_informational());
    assert!(report.entries[0].explanation.limitations.contains(&t!("view.page_server_reads.limit.identity")));
    assert!(report.limitations.contains(&t!("view.page_server_reads.limit.references")));
    assert_eq!(report.reference_coverage.as_deref(), Some(&evidence.reference_coverage));
    assert!(serde_json::to_value(report).unwrap().get("total_executions").is_none());
}

#[test]
fn omitted_body_rows_keep_original_positions_and_semantic_gaps_independent() {
    let mut evidence = Evidence::new(3);
    unresolved(&mut evidence, 2);
    let rows = evidence.rows();
    let report = explain(&evidence, &rows[2..], false).unwrap();
    assert_eq!(report.selected_rows, Some(3));
    assert_eq!(report.observed_rows, 1);
    assert!(!report.read_complete);
    let PageServerReadEntryIdentity::RuntimeFragment { reference, .. } = &report.entries[0].identity else { panic!() };
    assert_eq!(reference.row_ordinal(), 2);
    assert!(report.limitations.contains(&t!("view.page_server_reads.limit.read")));
    assert!(report.limitations.contains(&t!("view.page_server_reads.limit.references")));
    assert!(explain(&evidence, &rows[2..], true).is_err());
    let duplicate = (0..2)
        .map(|_| RuntimeEvidence { reference: rows[2].reference.clone(), runtime: rows[2].runtime })
        .collect::<Vec<_>>();
    assert!(explain(&evidence, &duplicate, false).is_err());
}

#[test]
fn informational_statistics_reject_repaired_values_and_wrong_original_source_or_capture() {
    for change in 0..6 {
        let mut evidence = Evidence::new(1);
        unresolved(&mut evidence, 0);
        match change {
            0 => evidence.records[0].page_server_reads.as_mut().unwrap().last_pages = Some("0".into()),
            1 => evidence.source.source = "foreign-native-source".into(),
            2 => evidence.read.capture.observation_id = "different-original-capture".into(),
            3 => {
                let CapturedRecordReference::Informational { position } = &mut evidence.references[0] else { panic!() };
                position.row_ordinal = 1;
            }
            4 => evidence.records[0].last_execution_at = Some("2020-01-01T06:59:00+00:00".into()),
            _ => {
                evidence.reference_coverage =
                    ReferenceCoverage { integrity_complete: true, complete: true, ..Default::default() }
            }
        }
        assert!(explain(&evidence, &evidence.rows(), true).is_err(), "change {change}");
    }
}
