use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{IndexConfigurationCase, assert_table_metadata_capture, index_configuration_evidence},
};
use culprit_core::{SourceState, telemetry::*};
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn original_index_and_heap_configuration_keeps_each_partition_and_native_absence_without_recovery() {
    for case in IndexConfigurationCase::ALL {
        let (source, records) = index_configuration_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_table_metadata_capture(&capture, &source, &records);
        let analysis = culprit_engine::collected_resources::index_configuration(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.context.as_ref().unwrap().target, source.target);
        assert_eq!(analysis.report.native_rows, Some(records.len() as u32));
        assert_eq!(analysis.report.observed_rows, records.len() as u32);
        assert_eq!(fixture.recovery_object_reads(), opened);
        for entry in &analysis.report.entries {
            let original = records
                .iter()
                .find_map(|row| match row {
                    TelemetryRecord::Operational(row) if Some(&row.key) == entry.observation_key.as_ref() => Some(row),
                    _ => None,
                })
                .unwrap();
            let native = match &original.fact {
                OperationalFact::MssqlIndex(index) => index.configuration.as_ref().unwrap(),
                OperationalFact::MssqlTable(table) => table.heap_configuration.as_ref().unwrap(),
                _ => panic!(),
            };
            assert_eq!(entry.parent_object, native.parent_object);
            assert_eq!(entry.index_id, native.index_id);
            assert_eq!(entry.retained_partitions, native.partitions.len() as u32);
            for (partition, native) in entry.partitions.iter().zip(&native.partitions) {
                assert_eq!(partition.partition_id, native.partition_id);
                assert_eq!(partition.hobt_id, native.hobt_id);
            }
        }
        if case == IndexConfigurationCase::Mixed {
            assert_ne!(
                analysis.report.entries[0].partitions[0].fields[0].text,
                analysis.report.entries[0].partitions[1].fields[0].text
            );
        }
        if case == IndexConfigurationCase::Partial {
            assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
            assert!(analysis.report.entries[0].partitions_limited);
            assert!(!analysis.report.entries[0].partitions_complete);
        }
        if case == IndexConfigurationCase::Denied {
            assert_eq!(analysis.report.source_state, Some(SourceState::Unavailable));
            assert!(analysis.report.entries.is_empty());
        }
        let wrong = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        assert!(culprit_engine::collected_resources::index_configuration(&wrong).is_err());
    }
}
#[tokio::test]
async fn missing_original_configuration_body_remains_partial_without_native_empty_inference() {
    let (source, records) = index_configuration_evidence(IndexConfigurationCase::Mixed);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILY).await;
    let analysis = culprit_engine::collected_resources::index_configuration(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_rows, Some(1));
    assert_eq!(analysis.report.observed_rows, 0);
    assert_eq!(analysis.report.read_complete, Some(false));
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(analysis.report.entries.is_empty());
    assert_eq!(capture.metrics().recovery_objects, 0);
}
