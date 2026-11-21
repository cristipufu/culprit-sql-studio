mod interpretation;
use super::fixture::Fixture;
use super::*;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{ManagedInstanceResourceCase, managed_instance_resource_case},
};
#[tokio::test]
async fn original_mi_history_reuses_one_report_without_current_member_or_operational_pressure() {
    for case in ManagedInstanceResourceCase::ALL {
        let (source, rows) = managed_instance_resource_case(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::ResourceSamples]).await;
        let analysis = culprit_engine::collected_resources::managed_instance_resources(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.observed_rows, rows.len() as u32);
        assert_eq!(fixture.recovery_object_reads(), opened);
        let observations = rows
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::ResourceSample(row) => row,
                _ => panic!(),
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::managed_instance_resources::explain(
            culprit_checks::managed_instance_resources::ManagedInstanceResourceEvidence {
                source: &source,
                observations: &observations,
                read_complete: true,
            },
        )
        .unwrap();
        assert_eq!(analysis.report.as_ref(), &expected);
    }
}
#[tokio::test]
async fn missing_interval_body_preserves_original_native_population_without_recovery_or_healthy_zero() {
    let (source, rows) = managed_instance_resource_case(ManagedInstanceResourceCase::Complete);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::ResourceSamples);
    let capture = fixture.read_families(&[EvidenceFamily::ResourceSamples]).await;
    let report = culprit_engine::collected_resources::managed_instance_resources(&capture).unwrap().report;
    assert_eq!(report.recorded_rows, Some(1));
    assert_eq!(report.observed_rows, 0);
    assert_eq!(report.source_state, culprit_core::SourceState::Partial);
    assert!(!report.read_complete);
    assert!(report.intervals.is_empty());
    assert_eq!(capture.source(), &source);
}
