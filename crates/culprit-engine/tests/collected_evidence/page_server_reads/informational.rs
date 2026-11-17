use super::*;

#[tokio::test]
async fn unresolved_original_rows_keep_exact_statistics_and_positions_without_diagnostic_identity() {
    let mut case = page_server_read_evidence_cases().into_iter().find(|case| case.name == "native_fragment").unwrap();
    let TelemetryRecord::QueryRuntime(runtime) = &mut case.records[0] else { panic!() };
    runtime.key.references = StatementReferences::default();
    let fixture = Fixture::new(case.source.clone(), case.records).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    assert!(capture.is_read_complete());
    assert!(capture.reference_coverage().integrity_complete);
    assert!(!capture.reference_coverage().complete);
    assert_eq!(capture.captured_rows(TelemetryStream::QueryRuntime), Some(1));
    assert_eq!(capture.captured_rows(TelemetryStream::QueryWaits), None);
    let original = &capture.records()[0];
    assert!(original.row().record_id.is_none());
    assert!(
        DiagnosticEvidenceRef::from_export(capture.source(), capture.capture_manifest_sha256(), original.row(), 0)
            .is_err()
    );

    let analysis = collected_resources::page_server_reads(&capture, None).unwrap();
    assert_eq!(fixture.recovery_object_reads(), before);
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert!(analysis.report.read_complete, "physical body completeness does not invent semantic identity");
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert_eq!(analysis.report.reference_coverage.as_deref(), Some(capture.reference_coverage()));
    assert!(analysis.report.definition_read.is_none() && analysis.definition_context.is_none());
    let parent = analysis.report.runtime_read.as_ref().unwrap();
    assert_eq!(&parent.capture, capture.capture());
    assert_eq!(parent.manifest_sha256, capture.capture_manifest_sha256());
    assert_eq!(parent.captured_rows(TelemetryStream::QueryRuntime), Some(1));
    let PageServerReadEntryIdentity::RuntimeFragment { reference, native } = &analysis.report.entries[0].identity
    else {
        panic!()
    };
    let CapturedRecordReference::Informational { position } = reference.as_ref() else { panic!() };
    assert_eq!(position.row_ordinal, 0);
    assert_eq!(position.payload_sha256, original.native_payload_sha256());
    reference.validate_canonical_record(parent, capture.source(), &original.row().record).unwrap();
    assert!(native.references.query.is_none() && native.references.plan.is_none());
    assert_eq!(analysis.report.entries[0].executions, "9007199254740993");
    assert_eq!(
        analysis.report.entries[0]
            .explanation
            .metrics
            .iter()
            .find(|metric| metric.is("view.page_server_reads.metric.last"))
            .unwrap()
            .text
            .as_deref(),
        Some("9007199254740993")
    );
    assert!(
        analysis.report.entries[0]
            .explanation
            .limitations
            .contains(&culprit_core::t!("view.page_server_reads.limit.identity"))
    );
    assert!(analysis.report.limitations.contains(&culprit_core::t!("view.page_server_reads.limit.references")));
    assert_eq!(analysis.context.records[0].row.record_id, None);
    assert_eq!(analysis.context.records[0].native_payload_sha256, original.native_payload_sha256());
}

#[tokio::test]
async fn a_missing_archive_body_keeps_original_stream_counts_and_nonzero_informational_ordinals() {
    let case = page_server_read_evidence_cases().into_iter().find(|case| case.name == "separate_fragments").unwrap();
    let fixture = Fixture::new(case.source, case.records).await;
    fixture.remove_first_runtime();
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    assert!(!capture.is_read_complete());
    assert!(!capture.reference_coverage().complete);
    let analysis = collected_resources::page_server_reads(&capture, None).unwrap();
    assert_eq!(analysis.report.selected_rows, Some(5));
    assert_eq!(analysis.report.observed_rows, 4);
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.report.runtime_read.as_ref().unwrap().captured_rows(TelemetryStream::QueryRuntime), Some(5));
    let positions = analysis
        .report
        .entries
        .iter()
        .map(|entry| {
            let PageServerReadEntryIdentity::RuntimeFragment { reference, .. } = &entry.identity else { panic!() };
            assert!(reference.is_informational());
            reference.row_ordinal()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(positions, [1, 2, 3, 4].into());
    assert!(analysis.report.limitations.contains(&culprit_core::t!("view.page_server_reads.limit.read")));
    assert!(analysis.report.limitations.contains(&culprit_core::t!("view.page_server_reads.limit.references")));
    assert_eq!(fixture.recovery_object_reads(), before);
}

#[tokio::test]
async fn unresolved_zero_execution_or_plan_references_never_qualify_trusted_denominators() {
    for missing_query in [true, false] {
        let mut case = page_server_read_evidence_cases()
            .into_iter()
            .find(|case| case.name == "single_fragment_with_definitions")
            .unwrap();
        let TelemetryRecord::QueryRuntime(original) = &mut case.records[0] else { panic!() };
        original.executions = "1".into();
        let mut unresolved = original.clone();
        unresolved.key.native.id = "unresolved-second-fragment".into();
        if missing_query {
            unresolved.key.references = StatementReferences::default();
            unresolved.executions = "0".into();
        } else {
            let plan = unresolved.key.references.plan.as_mut().unwrap();
            plan.native.id = "5".into();
            plan.definition_id = None;
            unresolved.key.native.qualifiers.insert("plan_id".into(), "5".into());
        }
        let control = Fixture::new(case.source.clone(), case.records.clone()).await;
        let captured = control.read().await;
        assert!(captured.reference_coverage().complete);
        let baseline = culprit_engine::collected_evidence::analyze(&captured, Some(&NativeIdentity::new("1"))).unwrap();
        assert!(baseline.analyses[0].workload.is_some(), "resolved positive control must qualify a denominator");

        case.records.push(TelemetryRecord::QueryRuntime(unresolved));
        let fixture = Fixture::new(case.source, case.records).await;
        let capture = fixture.read().await;
        assert!(capture.is_read_complete());
        assert!(capture.reference_coverage().integrity_complete && !capture.reference_coverage().complete);
        let analysis = culprit_engine::collected_evidence::analyze(&capture, Some(&NativeIdentity::new("1"))).unwrap();
        assert!(analysis.analyses[0].workload.is_none(), "missing_query={missing_query}");
        assert!(analysis.analyses[0].limitations.iter().any(|limitation| limitation.reason
            == culprit_core::collected_analysis::CollectedAnalysisLimit::RuntimeDenominatorUnavailable));
        assert_eq!(
            analysis
                .context
                .records
                .iter()
                .filter(|record| record.row.record.stream() == TelemetryStream::QueryRuntime)
                .count(),
            2
        );
        let runtime = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
        let report = collected_resources::page_server_reads(&runtime, None).unwrap().report;
        assert_eq!(report.entries.len(), 2);
        let PageServerReadEntryIdentity::RuntimeFragment { reference, .. } = &report.entries[1].identity else {
            panic!()
        };
        assert!(reference.is_informational());
        assert_eq!(report.entries[1].executions, if missing_query { "0" } else { "1" });
    }
}
