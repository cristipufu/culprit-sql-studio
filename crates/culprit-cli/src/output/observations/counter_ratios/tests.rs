use super::*;
use culprit_collector::analytical::test_support::{CounterRatioCase, counter_ratio_evidence};
use culprit_core::{SourceOutcome, telemetry::*};

fn report(case: CounterRatioCase) -> CounterRatioReport {
    let (source, records) = counter_ratio_evidence(case);
    let samples = records
        .into_iter()
        .map(|record| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::CounterSet(sample), ..
            }) = record
            else {
                panic!()
            };
            *sample
        })
        .collect::<Vec<_>>();
    let native = &source.observation.snapshot().unwrap().sources[0];
    let outcome =
        SourceOutcome { source: native.source.clone(), state: native.state, reason: None, reason_message: None };
    culprit_checks::counter_ratios::explain(&samples, Some(&outcome), true).unwrap()
}

#[test]
fn counter_ratio_text_keeps_exact_native_values_original_clock_and_every_aggregate_qualification() {
    let report = report(CounterRatioCase::Exact);
    let text = counter_ratios(&report);
    assert!(text.contains("9007199254740993"));
    assert!(!text.contains("9007199254740992"));
    assert!(text.contains("99.99%"));
    assert!(text.contains("2020-01-01T12:00:00.5Z"));
    assert!(text.contains("actual-native-cache-object"));
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(!text.contains("fixture.invalid"));
}

#[test]
fn missing_negative_zero_and_permission_states_are_distinct_and_never_manufacture_a_percentage() {
    assert!(counter_ratios(&report(CounterRatioCase::ZeroNumerator)).contains("0.00%"));
    assert!(counter_ratios(&report(CounterRatioCase::Negative)).contains("-1"));
    for case in [
        CounterRatioCase::ZeroBase,
        CounterRatioCase::MissingBase,
        CounterRatioCase::MissingBoth,
        CounterRatioCase::Negative,
        CounterRatioCase::OutsideSubset,
        CounterRatioCase::PermissionDenied,
    ] {
        let report = report(case);
        let text = counter_ratios(&report);
        assert!(!text.contains("100.00%"));
        assert!(!text.contains("0.00%"));
        assert!(text.contains(&report.summary));
    }
}
