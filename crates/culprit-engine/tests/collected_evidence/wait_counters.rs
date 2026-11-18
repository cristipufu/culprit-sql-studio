use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, WaitCounterCase},
};
use culprit_core::{Snapshot, SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn authentic_wait_captures_and_saved_scans_share_exact_rows_counts_clocks_and_qualification() {
    for case in WaitCounterCase::ALL {
        let (source, records) = test_support::wait_counter_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::WaitCounterSamples]).await;
        assert!(capture.is_read_complete(), "{case:?}");
        let analysis = collected_resources::wait_counters(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len());
        let rows = records
            .iter()
            .map(|record| {
                let TelemetryRecord::WaitCounter(row) = record else { panic!() };
                row.clone()
            })
            .collect();
        let read = WaitCounterSnapshot::from_source(&source, rows, true).unwrap();
        let expected = culprit_checks::wait_counters::explain(&read, source.target.engine()).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(&expected).unwrap());
        for (actual, original) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), original.clone()).unwrap());
        }
        let mut scan: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        // The independent scan carrier has client acquisition bounds, not the archive's native source authority.
        let mut scan_read = read;
        scan_read.context = Some(ObservationReadContext::Collector {
            observed_from: "2026-11-17T12:00:00.000000001Z".into(),
            observed_until: "2026-11-17T12:00:00.000000009Z".into(),
        });
        scan.pressure.get_or_insert_with(Default::default).wait_counters = Some(Box::new(scan_read));
        scan.format = culprit_core::SNAPSHOT_FORMAT;
        let saved = culprit_checks::analyze(scan);
        let report = saved.wait_counters.as_ref().unwrap();
        assert_eq!(
            serde_json::to_value(&report.entries).unwrap(),
            serde_json::to_value(&analysis.report.entries).unwrap()
        );
        assert_eq!(report.native_rows, analysis.report.native_rows);
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
        if case == WaitCounterCase::Exact {
            let directory = tempfile::tempdir().unwrap();
            culprit_engine::reports::save(directory.path(), &saved).unwrap();
            let reloaded = culprit_engine::reports::latest(directory.path(), None).unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(&reloaded.snapshot.pressure.as_ref().unwrap().wait_counters).unwrap(),
                serde_json::to_value(&saved.snapshot.pressure.as_ref().unwrap().wait_counters).unwrap()
            );
            let display = serde_json::to_value(culprit_engine::reports::display(&reloaded)).unwrap();
            assert_eq!(display["wait_counters"], serde_json::to_value(report).unwrap());
            assert!(analysis.report.entries.iter().any(|entry| entry.title == "SLEEP_TASK"));
            assert!(serde_json::to_string(&analysis).unwrap().contains("9007199254740993"));
        }
        if case == WaitCounterCase::Denied {
            assert_eq!(analysis.report.sources[0].state, SourceState::Unavailable);
        }
    }
}

#[tokio::test]
async fn missing_warehouse_wait_rows_keep_original_native_counts_and_read_gap_without_hydration() {
    let (source, rows) = test_support::wait_counter_evidence(WaitCounterCase::Exact);
    let expected = rows.len();
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::WaitCounterSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::WaitCounterSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.captured_rows(TelemetryStream::WaitCounterSamples), Some(expected as u32));
    let analysis = collected_resources::wait_counters(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!((analysis.report.native_rows, analysis.report.observed_rows), (expected as u32, expected as u32 - 1));
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.report.sources[0].state, SourceState::Partial);
    assert_eq!(fixture.recovery_object_reads(), opened);
}
