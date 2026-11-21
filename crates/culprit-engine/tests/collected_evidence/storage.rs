use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, StorageCase},
};
use culprit_core::{SourceState, telemetry::*};
#[tokio::test]
async fn original_shared_tempdb_log_replay_preserves_data_separation_exact_free_bytes_and_independent_missingness() {
    for case in [
        StorageCase::SharedTempdbLog,
        StorageCase::SharedTempdbLogUnknown,
        StorageCase::SharedTempdbLogPartial,
        StorageCase::SharedTempdbLogDenied,
    ] {
        let (source, rows) = test_support::storage_evidence(case);
        let fixture = Fixture::new(source.clone(), rows).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let analysis = culprit_engine::collected_resources::storage(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert!(matches!(analysis.report.target, CollectionTarget::Shared(_)));
        assert!(analysis.report.entries.iter().all(|entry| entry.scope == culprit_core::FileScope::Server));
        let data = analysis
            .report
            .entries
            .iter()
            .find(|entry| entry.source == culprit_core::source_id::TEMP_CAPACITY)
            .unwrap();
        assert_eq!(data.fields.iter().find(|field| field.key == "used_bytes").unwrap().value.as_deref(), Some("4096"));
        assert!(!data.fields.iter().any(|field| field.key == "allocated_free_bytes"));
        let log = analysis.report.entries.iter().find(|entry| entry.source == culprit_core::source_id::LOG_CAPACITY);
        if case == StorageCase::SharedTempdbLogDenied {
            assert!(log.is_none());
            assert_eq!(analysis.report.source_state, SourceState::Partial);
            assert_eq!(
                analysis
                    .report
                    .native_sources
                    .iter()
                    .find(|source| source.source == culprit_core::source_id::LOG_CAPACITY)
                    .unwrap()
                    .state,
                SourceState::Unavailable
            );
        } else {
            let log = log.unwrap();
            assert_eq!(log.observation_key, "shared-tempdb-log");
            let free = log.fields.iter().find(|field| field.key == "allocated_free_bytes").unwrap().value.as_deref();
            assert_eq!(free, (case != StorageCase::SharedTempdbLogUnknown).then_some("9007199254740992"));
            assert!(log.explanation.contains(&culprit_core::t!("view.storage.temp_log.limit")));
            if case != StorageCase::SharedTempdbLogUnknown {
                assert!(log.explanation.contains(&culprit_core::t!("view.storage.temp_log.active")));
            }
            if case == StorageCase::SharedTempdbLogPartial {
                assert_eq!(analysis.report.source_state, SourceState::Partial);
            }
        }
    }
}

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
        test_support::assert_storage_file_summary(&analysis.report, case);
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

#[tokio::test]
async fn saved_storage_reanalysis_retains_the_original_census_and_withdraws_it_indivisibly_for_sharing() {
    let (source, rows) = test_support::storage_evidence(StorageCase::FileCensus);
    let fixture = Fixture::new(source.clone(), rows.clone()).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let expected = culprit_engine::collected_resources::storage(&capture).unwrap();
    let mut snapshot: culprit_core::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.operations.storage = Some(Box::new(
        StorageRead::from_source(
            &source,
            rows.into_iter()
                .map(|row| {
                    let TelemetryRecord::Operational(row) = row else { unreachable!() };
                    row
                })
                .collect(),
            true,
        )
        .unwrap(),
    ));
    // The fixture attaches one independent original read, not a chart projection from the demo's old sources.
    snapshot.operations.spaces.clear();
    let raw = serde_json::to_vec(&snapshot).unwrap();
    let mut restored: culprit_core::Snapshot = serde_json::from_slice(&raw).unwrap();
    restored.server.database = "later-scan-database".into();
    let report = culprit_checks::analyze(restored.clone())
        .storage
        .expect("the actual saved-report pipeline reuses the original read");
    assert_eq!(serde_json::to_value(&report).unwrap(), serde_json::to_value(expected.report).unwrap());
    assert_eq!(report.target, source.target);
    let shared = culprit_core::redact::redact_snapshot(&restored);
    assert!(shared.operations.storage.is_none());
    assert!(culprit_checks::analyze(shared.clone()).storage.is_none());
    assert!(
        shared.sources.iter().any(|source| source.source == culprit_core::source_id::FILE_CAPACITY
            && source.state == SourceState::Unavailable)
    );
}
