use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{IndexedViewsCase, assert_table_metadata_capture, indexed_views_evidence},
};
use culprit_core::{SourceState, telemetry::*};
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
#[tokio::test]
async fn indexed_views_reuse_original_index_catalog_rows_and_keep_table_population_separate() {
    for case in IndexedViewsCase::ALL {
        let (source, records) = indexed_views_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_table_metadata_capture(&capture, &source, &records);
        let analysis = culprit_engine::collected_resources::indexed_views(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.context.as_ref().unwrap().target, source.target);
        assert_eq!(analysis.report.native_index_rows, Some(records.len() as u32));
        assert_eq!(analysis.report.observed_index_rows, records.len() as u32);
        assert_eq!(fixture.recovery_object_reads(), opened);
        if matches!(case, IndexedViewsCase::Present | IndexedViewsCase::UsageNotCollected) {
            assert_eq!(analysis.report.entries.len(), 2);
            assert_eq!(analysis.report.observed_index_rows, 3);
            for entry in &analysis.report.entries {
                let original = records
                    .iter()
                    .find_map(|record| match record {
                        TelemetryRecord::Operational(row) if Some(&row.key) == entry.observation_key.as_ref() => {
                            Some(row)
                        }
                        _ => None,
                    })
                    .unwrap();
                let OperationalFact::MssqlIndex(index) = &original.fact else { panic!() };
                assert_eq!(index.parent_kind, MssqlIndexParentKind::IndexedView);
                assert_eq!(entry.parent_object, index.parent_object);
                assert_eq!(entry.usage, index.usage);
                assert_eq!(entry.configuration.as_ref().unwrap().parent_kind, MssqlIndexParentKind::IndexedView);
            }
        } else {
            assert!(analysis.report.entries.is_empty());
        }
        if case == IndexedViewsCase::Partial {
            assert_eq!(analysis.report.source.state, SourceState::Partial);
        }
        if case == IndexedViewsCase::Denied {
            assert_eq!(analysis.report.source.state, SourceState::Unavailable);
        }
        let wrong = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        assert!(culprit_engine::collected_resources::indexed_views(&wrong).is_err());
    }
}
#[tokio::test]
async fn a_missing_original_view_row_keeps_the_native_population_and_partial_receipt_without_recovery() {
    let (source, records) = indexed_views_evidence(IndexedViewsCase::Present);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_row(TelemetryStream::OperationalSamples, 1);
    let capture = fixture.read_families(FAMILY).await;
    let report = culprit_engine::collected_resources::indexed_views(&capture).unwrap().report;
    assert_eq!(report.native_index_rows, Some(3));
    assert_eq!(report.observed_index_rows, 2);
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.read_complete, Some(false));
    assert_eq!(report.source.state, SourceState::Partial);
    assert_eq!(capture.metrics().recovery_objects, 0);
}
