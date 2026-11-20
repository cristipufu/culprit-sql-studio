use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{TableFullTextCase, assert_table_metadata_capture, table_full_text_evidence},
};
use culprit_core::{SourceState, telemetry::*};
use culprit_engine::collected_resources;
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn original_cdc_enrollment_replays_without_inventing_job_progress_or_live_reads() {
    use culprit_collector::analytical::test_support::{TableCdcCase, table_cdc_evidence};
    for case in TableCdcCase::ALL {
        let (source, records) = table_cdc_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let recovery_reads = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_table_metadata_capture(&capture, &source, &records);
        assert_eq!(fixture.recovery_object_reads(), recovery_reads);
    }
}

#[tokio::test]
async fn original_table_catalog_states_share_the_scan_interpretation_without_current_database_or_recovery() {
    for case in TableFullTextCase::ALL {
        let (source, records) = table_full_text_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_table_metadata_capture(&capture, &source, &records);
        let analysis = collected_resources::table_full_text(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.native_rows, Some(records.len() as u32));
        assert_eq!(analysis.report.observed_rows, records.len() as u32);
        assert_eq!(
            analysis.report.observed_from.as_deref(),
            Some(source.observation.snapshot().unwrap().observed_from.as_str())
        );
        assert_eq!(fixture.recovery_object_reads(), opened);
        for entry in &analysis.report.entries {
            let original = records
                .iter()
                .find_map(|record| match record {
                    TelemetryRecord::Operational(row) if Some(&row.key) == entry.observation_key.as_ref() => Some(row),
                    _ => None,
                })
                .unwrap();
            let OperationalFact::MssqlTable(table) = &original.fact else { panic!() };
            assert_eq!(table.schema, entry.schema);
            assert_eq!(table.name, entry.table);
            let mut scan: culprit_core::Snapshot = serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/mssql-demo.json"
            )))
            .unwrap();
            let scan_table = &mut scan.tables[0];
            scan_table.schema = table.schema.clone();
            scan_table.name = table.name.clone();
            scan_table.mssql_full_text = Some(table.full_text.clone());
            let mut expected =
                culprit_engine::explain_table_full_text(&scan, &scan.tables[0]).unwrap().unwrap().entries.remove(0);
            expected.observation_key = entry.observation_key.clone();
            assert_eq!(serde_json::to_value(entry).unwrap(), serde_json::to_value(expected).unwrap());
            if table.full_text.index.is_none() {
                assert!(entry.metrics.is_empty());
            }
        }
        if case == TableFullTextCase::SourceDenied {
            assert_eq!(analysis.report.source_state, Some(SourceState::Unavailable));
            assert!(analysis.report.entries.is_empty());
        }
        if case == TableFullTextCase::Partial {
            assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
        }
        let wrong = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        assert!(collected_resources::table_full_text(&wrong).is_err());
    }
}

#[tokio::test]
async fn a_missing_table_body_is_unknown_and_cannot_become_a_no_index_row_or_a_complete_empty_catalog() {
    let (source, records) = table_full_text_evidence(TableFullTextCase::Enabled);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::table_full_text(&capture).unwrap();
    assert_eq!(analysis.report.native_rows, Some(1));
    assert_eq!(analysis.report.observed_rows, 0);
    assert_eq!(analysis.report.read_complete, Some(false));
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(analysis.report.entries.is_empty());
    assert!(analysis.report.limitations.contains(&culprit_core::t!("view.table_full_text.limit.read")));
    assert_eq!(analysis.context.source, source);
    assert_eq!(capture.metrics().recovery_objects, 0);
}

#[tokio::test]
async fn original_table_storage_configuration_replays_exactly_without_current_metadata_or_size_inference() {
    use culprit_collector::analytical::test_support::{TableStorageCase, table_storage_evidence};
    for case in TableStorageCase::ALL {
        let (source, records) = table_storage_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_table_metadata_capture(&capture, &source, &records);
        assert_eq!(fixture.recovery_object_reads(), opened);
        for row in capture.records() {
            let TelemetryRecord::Operational(row) = &row.row().record else { panic!() };
            let OperationalFact::MssqlTable(table) = &row.fact else { panic!() };
            assert!(table.is_valid());
            assert!(table.estimated_rows.is_none());
            assert!(table.reserved_bytes.is_none());
            assert_eq!(table.identity.database_id, 7);
        }
        let source_state = capture.source().observation.snapshot().unwrap().sources[0].state;
        if case == TableStorageCase::SourceDenied {
            assert_eq!(source_state, SourceState::Unavailable);
            assert!(capture.records().is_empty());
        }
        if case == TableStorageCase::Partial {
            assert_eq!(source_state, SourceState::Partial);
        }
    }
}
