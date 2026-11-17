use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, CounterRatioCase},
};
use culprit_core::{Snapshot, SourceOutcome, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn captured_counter_ratios_reuse_the_scan_view_and_preserve_original_exact_inputs_and_scope() {
    for case in CounterRatioCase::ALL {
        let (source, records) = test_support::counter_ratio_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete());
        let analysis = collected_resources::counter_ratios(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len());
        let samples = records
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::CounterSet(set), ..
                }) = record
                else {
                    panic!()
                };
                set.as_ref().clone()
            })
            .collect::<Vec<_>>();
        let native = source.observation.snapshot().unwrap();
        let coverage = &native.sources[0];
        let outcome = SourceOutcome {
            source: coverage.source.clone(),
            state: coverage.state,
            reason: None,
            reason_message: None,
        };
        // The SDK may add its source failure reason; the typed rows and all explanation values share one pure view.
        let expected = culprit_checks::counter_ratios::explain(&samples, Some(&outcome), true).unwrap();
        assert_eq!(
            serde_json::to_value(&analysis.report.entries).unwrap(),
            serde_json::to_value(expected.entries).unwrap()
        );
        for (actual, original) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), original.clone()).unwrap());
        }
        if case == CounterRatioCase::Exact {
            let mut scan: Snapshot =
                serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
            scan.pressure.get_or_insert_with(Default::default).resource_counters = Some(samples);
            scan.sources.retain(|source| source.source != outcome.source);
            scan.sources.push(outcome);
            let report = culprit_checks::analyze(scan).counter_ratios.unwrap();
            assert_eq!(
                serde_json::to_value(&report.entries).unwrap(),
                serde_json::to_value(&analysis.report.entries).unwrap()
            );
            assert!(serde_json::to_string(&report).unwrap().contains("9007199254740993"));
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn a_missing_original_body_remains_partial_even_with_recovery_available() {
    let (source, records) = test_support::counter_ratio_evidence(CounterRatioCase::Exact);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = collected_resources::counter_ratios(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.report.source_state, Some(culprit_core::SourceState::Partial));
    assert!(analysis.report.entries.is_empty());
    assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(1));
}
