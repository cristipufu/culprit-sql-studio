use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{assert_page_server_read_capture, page_server_read_evidence_cases},
};
use culprit_core::{SourceState, page_server_read_report::PageServerReadEntryIdentity, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn original_runtime_fragments_reuse_the_pure_explanation_with_no_definition_or_recovery_read() {
    for case in page_server_read_evidence_cases() {
        let fixture = Fixture::new(case.source.clone(), case.records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
        assert_page_server_read_capture(&capture, &case.source, &case.records);
        let analysis = collected_resources::page_server_reads(&capture, None).unwrap();
        assert_eq!(analysis.context.source, case.source);
        let native_rows = case.records.iter().filter(|row| row.stream() == TelemetryStream::QueryRuntime).count();
        assert_eq!(analysis.context.records.len(), native_rows);
        assert_eq!(analysis.report.entries.len(), native_rows, "{}", case.name);
        assert!(analysis.report.read_complete);
        let read = analysis.report.runtime_read.as_deref().unwrap();
        assert_eq!(analysis.report.reference_coverage.as_deref(), Some(capture.reference_coverage()));
        assert_eq!(fixture.recovery_object_reads(), before, "{}", case.name);
        assert_eq!(capture.metrics().recovery_objects, 0);
        let rows = capture
            .records()
            .iter()
            .zip(&analysis.report.entries)
            .map(|(record, entry)| {
                let TelemetryRecord::QueryRuntime(runtime) = &record.row().record else { panic!() };
                let PageServerReadEntryIdentity::RuntimeFragment { reference, native } = &entry.identity else {
                    panic!()
                };
                reference.validate_canonical_record(read, &case.source, &record.row().record).unwrap();
                assert_eq!(native.as_ref(), &runtime.key);
                culprit_checks::page_server_reads::RuntimeEvidence { reference: reference.as_ref().clone(), runtime }
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::page_server_reads::explain_archive(
            culprit_checks::page_server_reads::PageServerReadEvidence {
                source: &case.source,
                read,
                reference_coverage: capture.reference_coverage(),
                rows: &rows,
                selected_rows: capture.metrics().selected_rows,
                read_complete: true,
                definitions: None,
            },
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        if case.name == "native_fragment" {
            let entry = &analysis.report.entries[0];
            assert_eq!(entry.executions, "9007199254740993");
            assert_eq!(
                entry
                    .explanation
                    .metrics
                    .iter()
                    .find(|metric| metric.is("view.page_server_reads.metric.last"))
                    .unwrap()
                    .text
                    .as_deref(),
                Some("9007199254740993")
            );
            assert!(entry.explanation.limitations.contains(&culprit_core::t!("view.page_server_reads.limit.origin")));
        }
    }
}

mod informational;

#[tokio::test]
async fn exact_one_fragment_plan_projection_retains_the_same_pure_statistics_without_using_display_ids_as_authority() {
    let case = page_server_read_evidence_cases()
        .into_iter()
        .find(|case| case.name == "single_fragment_with_definitions")
        .unwrap();
    let fixture = Fixture::new(case.source, case.records).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = culprit_engine::collected_evidence::analyze(&capture, Some(&NativeIdentity::new("1"))).unwrap();
    let workload = analysis.analyses[0].workload.as_ref().unwrap();
    let plan = &workload.queries[0].plans[0];
    assert_ne!(plan.id, plan.native_identity.as_ref().unwrap().plan_id, "archive display labels remain definition ids");
    let summary = plan.page_server_reads.as_ref().unwrap();
    assert_eq!(summary.statistics().last_pages.as_deref(), Some("17"));
    let report = culprit_checks::page_server_reads::explain_workload(workload, None).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].executions, "1");
    assert_eq!(fixture.recovery_object_reads(), before);
}

#[tokio::test]
async fn generic_multi_fragment_plan_projection_discloses_omission_while_raw_explanations_keep_both_last_values() {
    let mut case = page_server_read_evidence_cases()
        .into_iter()
        .find(|case| case.name == "single_fragment_with_definitions")
        .unwrap();
    let mut second = case.records[0].clone();
    let TelemetryRecord::QueryRuntime(runtime) = &mut second else { panic!() };
    runtime.key.native.id = "18".into();
    runtime.page_server_reads = Some(Box::new(PageServerReadStatistics {
        availability: PageServerReadAvailability::Available,
        average_pages: Some(0.0),
        last_pages: Some("0".into()),
        minimum_pages: Some("0".into()),
        maximum_pages: Some("0".into()),
        standard_deviation_pages: Some(0.0),
    }));
    case.records.push(second);
    let fixture = Fixture::new(case.source, case.records).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read().await;
    let analysis = culprit_engine::collected_evidence::analyze(&capture, Some(&NativeIdentity::new("1"))).unwrap();
    let selected = &analysis.analyses[0];
    assert!(selected.workload.as_ref().unwrap().queries[0].plans[0].page_server_reads.is_none());
    assert!(
        selected
            .limitations
            .iter()
            .any(|limit| limit.reason == culprit_core::collected_analysis::CollectedAnalysisLimit::MetricNotProjected)
    );
    let runtime = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    let original = collected_resources::page_server_reads(&runtime, None).unwrap();
    let last = original
        .report
        .entries
        .iter()
        .map(|entry| {
            entry
                .explanation
                .metrics
                .iter()
                .find(|metric| metric.is("view.page_server_reads.metric.last"))
                .unwrap()
                .text
                .clone()
                .unwrap()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(last, ["0".to_string(), "17".to_string()].into_iter().collect::<std::collections::BTreeSet<_>>());
    assert_eq!(fixture.recovery_object_reads(), before);
}

#[tokio::test]
async fn tied_clocks_keep_native_fragments_plan_outcome_and_replica_groups_independent() {
    let case = page_server_read_evidence_cases().into_iter().find(|case| case.name == "separate_fragments").unwrap();
    let fixture = Fixture::new(case.source.clone(), case.records).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    let analysis = collected_resources::page_server_reads(&capture, None).unwrap();
    let keys = analysis
        .report
        .entries
        .iter()
        .map(|entry| {
            let PageServerReadEntryIdentity::RuntimeFragment { native, .. } = &entry.identity else { panic!() };
            (native.native.id.clone(), native.native.qualifiers.clone())
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), 5);
    assert_eq!(
        keys.iter()
            .filter(|(_, dimensions)| dimensions["plan_id"] == "4"
                && dimensions["execution_type"] == "0"
                && dimensions["replica_group_id"] == "1")
            .count(),
        2
    );
    assert!(keys.iter().any(|(_, dimensions)| dimensions["execution_type"] == "3"));
    assert!(keys.iter().any(|(_, dimensions)| dimensions["replica_group_id"] == "2"));
    assert!(keys.iter().any(|(_, dimensions)| dimensions["plan_id"] == "5"));
    assert_eq!(fixture.recovery_object_reads(), before);
}

#[tokio::test]
async fn missing_rows_or_publication_keep_the_original_selection_as_an_explicit_gap() {
    for unpublished in [false, true] {
        let case = page_server_read_evidence_cases().remove(0);
        let fixture = Fixture::new(case.source.clone(), case.records).await;
        if unpublished {
            fixture.unpublish();
        } else {
            fixture.remove_first_runtime();
        }
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
        assert!(!capture.is_read_complete());
        let analysis = collected_resources::page_server_reads(&capture, None).unwrap();
        assert_eq!(analysis.context.source, case.source);
        assert!(analysis.report.entries.is_empty());
        assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
        assert!(analysis.report.limitations.contains(&culprit_core::t!("view.page_server_reads.limit.read")));
        assert_eq!(fixture.recovery_object_reads(), before);
    }
}

#[tokio::test]
async fn runtime_not_collected_and_unrelated_families_cannot_counterfeit_remote_page_zero() {
    let mut case = page_server_read_evidence_cases().remove(0);
    let SourceObservation::QueryHistory(history) = &mut case.source.observation else { panic!() };
    history.native_properties.remove(PageServerReadAvailability::PROPERTY);
    for row in &mut case.records {
        let TelemetryRecord::QueryRuntime(runtime) = row else { panic!() };
        runtime.page_server_reads = None;
    }
    let fixture = Fixture::new(case.source, case.records).await;
    let capture = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    let analysis = collected_resources::page_server_reads(&capture, None).unwrap();
    assert_eq!(analysis.report.entries[0].explanation.availability, PageServerReadAvailability::Unknown);
    assert!(analysis.report.entries[0].explanation.metrics.is_empty());
    let mixed = fixture.read_families(&[EvidenceFamily::QueryRuntime, EvidenceFamily::QueryWaits]).await;
    assert!(
        collected_resources::page_server_reads(&mixed, None).is_err(),
        "selected_rows must describe exactly the requested runtime family"
    );
}
