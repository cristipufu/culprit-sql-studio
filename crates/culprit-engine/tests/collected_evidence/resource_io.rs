use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support,
};
use culprit_core::telemetry::*;

#[tokio::test]
async fn sdk_readback_retains_native_accounting_scope_and_exact_historical_wait_mechanisms() {
    let mut cases: Vec<_> = [DataIoAccounting::Unknown, DataIoAccounting::DataFiles, DataIoAccounting::ComputeLocalSsd]
        .into_iter()
        .map(|basis| {
            let (source, rows) = test_support::resource_io_evidence(basis);
            (source, rows, EvidenceFamily::ResourceSamples)
        })
        .collect();
    let (source, rows) = test_support::rbio_wait_evidence();
    cases.push((source, rows, EvidenceFamily::WaitCounterSamples));
    for (source, rows, family) in cases {
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[family]).await;
        assert!(capture.is_read_complete());
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.records().len(), rows.len());
        for (actual, native) in capture.records().iter().zip(rows) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), native).unwrap());
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}

#[tokio::test]
async fn sdk_missing_counter_rows_are_partial_without_fabricated_zero_or_changed_native_coverage() {
    let (source, rows) = test_support::rbio_wait_evidence();
    let fixture = Fixture::new(source.clone(), rows.clone()).await;
    fixture.remove_first_row(TelemetryStream::WaitCounterSamples);
    let capture = fixture.read_families(&[EvidenceFamily::WaitCounterSamples]).await;
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.captured_rows(TelemetryStream::WaitCounterSamples), Some(rows.len() as u32));
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.records().len(), rows.len() - 1);
}
