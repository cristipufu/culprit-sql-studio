use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::maintenance_progress_evidence_cases,
};
use culprit_core::{SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn native_maintenance_points_reuse_the_pure_report_and_keep_original_context_once_without_payload_reads() {
    let cases = maintenance_progress_evidence_cases();
    assert_eq!(cases.len(), 8);
    for (source, records) in cases {
        let SourceObservation::MaintenanceProgress { snapshot, result } = &source.observation else { panic!() };
        let raw = MaintenanceProgressSnapshot {
            source: snapshot.sources[0].source.clone(),
            context: result.read_context(&source, snapshot),
            coverage: snapshot.sources[0].clone(),
            result: result.as_ref().clone(),
            observations: records
                .iter()
                .map(|row| {
                    let TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::MaintenanceProgress(point),
                        ..
                    }) = row
                    else {
                        panic!()
                    };
                    point.as_ref().clone()
                })
                .collect(),
        };
        let expected = culprit_checks::maintenance_progress::explain(&raw, true).unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let analysis = collected_resources::maintenance_progress(&capture).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len());
        assert_eq!(fixture.recovery_object_reads(), before);
        assert_eq!(analysis.context.metrics.recovery_objects, 0);
        assert!(
            analysis.context.records.iter().zip(&records).all(|(retained, original)| retained.row.record == *original)
        );
        let report = serde_json::to_value(&analysis.report).unwrap();
        for absent in ["snapshot", "workload", "sql", "completed_at", "resource_bytes", "percent"] {
            assert!(report.get(absent).is_none(), "{absent}");
        }
    }
}

#[tokio::test]
async fn missing_native_row_preserves_original_membership_and_count_without_fabricating_completion() {
    let (source, rows) = maintenance_progress_evidence_cases().into_iter().find(|(_, rows)| !rows.is_empty()).unwrap();
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let analysis = collected_resources::maintenance_progress(&capture).unwrap();
    assert!(analysis.report.entries.is_empty());
    assert_eq!(analysis.report.membership, MaintenanceProgressMembership::Observed);
    assert_eq!(analysis.report.native_rows, 1);
    assert_eq!(analysis.report.observed_rows, 0);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert_eq!(analysis.context.source, source);
    assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
}

#[tokio::test]
async fn ysql_estimate_below_done_and_pg_exact_counts_remain_distinct_without_rounded_metrics() {
    let mut saw_pg = false;
    let mut saw_ysql = false;
    for (source, records) in maintenance_progress_evidence_cases().into_iter().filter(|(_, rows)| !rows.is_empty()) {
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let report = collected_resources::maintenance_progress(&capture).unwrap().report;
        let entry = &report.entries[0];
        for metric in &entry.metrics {
            if metric.is("view.maintenance_progress.metric.blocks_total") {
                saw_pg = true;
                assert_eq!(metric.text.as_deref(), Some("9007199254740993"));
            }
            if metric.is("view.maintenance_progress.metric.ysql_done") {
                saw_ysql = true;
                assert_eq!(metric.text.as_deref(), Some("9007199254740993"));
            }
        }
    }
    assert!(saw_pg && saw_ysql);
}
