use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, StorageCase},
};
use culprit_core::{SourceState, telemetry::*};
#[tokio::test]
async fn authentic_storage_capture_keeps_exact_quantities_populations_file_meanings_and_original_receipts() {
    for case in StorageCase::ALL {
        let (source, rows) = test_support::storage_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::storage(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        let read = StorageRead::from_source(
            &source,
            rows.into_iter()
                .map(|row| {
                    let TelemetryRecord::Operational(row) = row else { panic!() };
                    row
                })
                .collect(),
            true,
        )
        .unwrap();
        let expected = culprit_checks::storage::explain(&read).unwrap();
        assert_eq!(serde_json::to_value(analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}
#[tokio::test]
async fn missing_file_body_preserves_original_count_with_no_recovery_hydration_or_healthy_zero() {
    let (source, rows) = test_support::storage_evidence(StorageCase::Exact);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = culprit_engine::collected_resources::storage(&capture).unwrap();
    assert_eq!(analysis.report.native_rows, 2);
    assert_eq!(analysis.report.observed_rows, 1);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert_eq!(analysis.context.source, source);
    assert_eq!(fixture.recovery_object_reads(), opened);
}
