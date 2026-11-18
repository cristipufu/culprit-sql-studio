use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{assert_operational_capture, hyperscale_log_component_evidence_cases},
};
use culprit_core::{SourceState, telemetry::*};
const FAMILIES: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
#[tokio::test]
async fn all_native_component_cases_use_the_same_view_and_preserve_exact_original_scope_and_clocks() {
    for (source, records) in hyperscale_log_component_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILIES).await;
        assert_operational_capture(&capture, &source, &records);
        let analysis = culprit_engine::collected_resources::hyperscale_log_components(&capture).unwrap();
        let observations = records
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row.clone()
            })
            .collect();
        let read = HyperscaleLogComponentRead::from_source(&source, observations, true).unwrap();
        let expected = culprit_checks::hyperscale_log_components::explain(&read).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(analysis.report.native_rows, records.len() as u32);
        assert_eq!(analysis.report.context.observed_from, "2026-11-18T12:00:00.000000001Z");
        assert!(analysis.report.context.database_writable.is_none());
    }
}
#[tokio::test]
async fn missing_body_keeps_original_component_count_and_never_becomes_a_complete_or_healthy_empty_point() {
    let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILIES).await;
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = culprit_engine::collected_resources::hyperscale_log_components(&capture).unwrap();
    assert_eq!(analysis.report.native_rows, 2);
    assert_eq!(analysis.report.observed_rows, 1);
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
}
