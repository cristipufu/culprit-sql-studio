use super::*;
use culprit_core::telemetry::*;

fn report(missing_body: bool) -> PageServerReadReport {
    let mut case = culprit_collector::analytical::test_support::page_server_read_evidence_cases()
        .into_iter()
        .find(|case| case.name == "native_fragment")
        .unwrap();
    let TelemetryRecord::QueryRuntime(runtime) = &mut case.records[0] else { panic!() };
    runtime.key.references.query.as_mut().unwrap().definition_id = None;
    runtime.key.references.plan.as_mut().unwrap().definition_id = None;
    let capture = CaptureIdentity {
        observation_id: "PRIVATE-CAPTURE-NOT-HUMAN-OUTPUT".into(),
        revision: Some(CaptureRevision {
            outbox_id: "PRIVATE-OUTBOX-NOT-HUMAN-OUTPUT".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let selected_rows = if missing_body { 2 } else { 1 };
    let read = CapturedReadRef::new(
        &case.source,
        capture.clone(),
        digest(b"controlled-original-manifest"),
        vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows: selected_rows }],
    )
    .unwrap();
    let original = ExportedRecord::new(&case.source, &capture, case.records.remove(0)).unwrap();
    assert!(original.record_id.is_none());
    let reference = CapturedRecordReference::from_export(&read, &case.source, &original, 0).unwrap();
    let TelemetryRecord::QueryRuntime(runtime) = &original.record else { panic!() };
    let rows = [culprit_checks::page_server_reads::RuntimeEvidence { reference, runtime }];
    let reference_coverage = ReferenceCoverage {
        queries: DefinitionCoverage { required: 1, resolved: 0, unresolved: 1 },
        plans: DefinitionCoverage { required: 1, resolved: 0, unresolved: 1 },
        integrity_complete: true,
        complete: false,
        ..Default::default()
    };
    culprit_checks::page_server_reads::explain_archive(culprit_checks::page_server_reads::PageServerReadEvidence {
        source: &case.source,
        read: &read,
        reference_coverage: &reference_coverage,
        rows: &rows,
        selected_rows,
        read_complete: !missing_body,
        definitions: None,
    })
    .unwrap()
}

#[test]
fn exact_native_statistics_and_original_positions_are_informational_without_printing_proof_inputs() {
    let report = report(false);
    assert!(report.read_complete);
    assert!(!report.reference_coverage.as_ref().unwrap().complete);
    let text = page_server_reads(&report);
    assert!(text.contains("9007199254740993") && !text.contains("9007199254740992"));
    assert!(text.contains(&t!(
        "evidence.cli.page_server_reads.position",
        stream = TelemetryStream::QueryRuntime.file_name(),
        row = "0"
    )));
    assert!(text.contains(&t!("view.page_server_reads.limit.identity")));
    assert!(text.contains(&t!("view.page_server_reads.limit.references")));
    assert!(text.contains(&t!("evidence.cli.source_partial")));
    assert!(!text.contains("PRIVATE-CAPTURE-NOT-HUMAN-OUTPUT"));
    assert!(!text.contains("PRIVATE-OUTBOX-NOT-HUMAN-OUTPUT"));
    assert!(!text.contains(&report.runtime_read.as_ref().unwrap().manifest_sha256));
}

#[test]
fn missing_body_and_statement_identity_remain_independent_qualifications_in_text_and_json() {
    let report = report(true);
    let text = page_server_reads(&report);
    assert!(text.contains(&t!("view.page_server_reads.limit.read")));
    assert!(text.contains(&t!("view.page_server_reads.limit.references")));
    assert!(text.contains(&t!("view.page_server_reads.limit.identity")));
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["read_complete"], false);
    assert_eq!(json["reference_coverage"]["integrity_complete"], true);
    assert_eq!(json["reference_coverage"]["complete"], false);
    assert_eq!(json["runtime_read"]["streams"][0]["rows"], 2);
    assert_eq!(json["entries"][0]["identity"]["reference"]["kind"], "informational");
    assert_eq!(json["entries"][0]["identity"]["reference"]["position"]["row_ordinal"], 0);
    assert!(json.get("total_executions").is_none());
}
