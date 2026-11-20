use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{assert_operational_capture, columnstore_configuration_evidence_cases},
};
use culprit_core::telemetry::*;

#[tokio::test]
async fn persisted_columnstore_configuration_keeps_exact_original_native_scope_nullable_values_and_states() {
    let cases = columnstore_configuration_evidence_cases();
    assert_eq!(cases.len(), 6);
    for (source, records) in cases {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert_operational_capture(&capture, &source, &records);
        let [row] = capture.records() else { panic!("one original index, not one row per column") };
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::MssqlIndex(index), .. }) =
            &row.row().record
        else {
            panic!("original native index")
        };
        assert!(index.is_valid());
        let configuration = index.columnstore.as_ref().unwrap();
        assert_eq!(configuration.table, index.table);
        assert_eq!(configuration.index_id, index.index_id);
        assert_eq!(configuration.columns.len(), index.columns.len());
        assert_eq!(fixture.recovery_object_reads(), before);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}
