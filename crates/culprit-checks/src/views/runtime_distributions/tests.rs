use super::*;
use culprit_core::{
    DistributionPopulation, ResetMarker, RuntimeDistribution, RuntimeDistributionOrigin, RuntimeDuration,
    RuntimeHistogramBucket, RuntimeQuantile,
    telemetry::{NativeIdentity, StatementRuntimeDistribution},
};

fn row(source: &str, histogram: bool) -> OperationalObservation {
    let distribution = if histogram {
        RuntimeDistribution::native_histogram(
            source,
            vec![RuntimeHistogramBucket {
                lower: RuntimeDuration::Picoseconds { value: "0".into() },
                upper: RuntimeDuration::Picoseconds { value: u64::MAX.to_string() },
                samples: "9007199254740993".into(),
            }],
        )
    } else {
        RuntimeDistribution {
            source: source.into(),
            origin: RuntimeDistributionOrigin::StatementSummary,
            population: DistributionPopulation::NativeCounters { sample_count: None },
            p50: None,
            p95: None,
            p99: Some(RuntimeQuantile::HistogramUpperBound {
                lower: None,
                upper: RuntimeDuration::Picoseconds { value: "1".into() },
            }),
            histogram: vec![],
            issue: None,
        }
    };
    let point = StatementRuntimeDistribution {
        identity: NativeIdentity::new("digest"),
        scope: MeasurementScope::Database,
        observed_at: if histogram { "2020-01-01T00:00:00.2Z" } else { "2020-01-01T00:00:00.1Z" }.into(),
        reset: ResetMarker::Unknown,
        distribution,
    };
    OperationalObservation {
        source: source.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::RuntimeDistribution(Box::new(point)),
    }
}
fn outcome(source: &str, state: SourceState) -> SourceOutcome {
    SourceOutcome { source: source.into(), state, reason: None, reason_message: None }
}

#[test]
fn independent_sources_remain_separate_and_exact_population_counts_do_not_borrow_summary_work() {
    let summary = row("summary", false);
    let histogram = row("histogram", true);
    let report = explain(
        &[&summary, &histogram],
        &[outcome("summary", SourceState::Partial), outcome("histogram", SourceState::Collected)],
    )
    .unwrap();
    assert_eq!(report.entries.len(), 2);
    let summary_entry = report.entries.iter().find(|entry| entry.observation_key == summary.key).unwrap();
    assert_eq!(summary_entry.metrics.len(), 1);
    let histogram_entry = report.entries.iter().find(|entry| entry.observation_key == histogram.key).unwrap();
    assert!(histogram_entry.metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993")));
    assert_ne!(summary_entry.observed_at, histogram_entry.observed_at);
    assert_eq!(report.sources.len(), 2);
    assert!(report.limitations.iter().any(|limitation| limitation.contains("independently")));
}

#[test]
fn empty_and_unavailable_sources_never_imply_a_healthy_distribution_or_parameter_cause() {
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::Partial] {
        let report = explain(&[], &[outcome("source", state)]).unwrap();
        assert!(report.entries.is_empty());
        assert_eq!(report.sources[0].state, state);
        assert!(report.summary.contains("does not establish"));
    }
}

#[test]
fn duplicate_unowned_or_undeclared_points_and_contradictory_coverage_are_rejected() {
    let row = row("source", true);
    assert!(explain(&[&row, &row], &[outcome("source", SourceState::Partial)]).is_err());
    assert!(explain(&[&row], &[outcome("other-source", SourceState::Partial)]).is_err());
    assert!(explain(&[&row], &[outcome("source", SourceState::Unavailable)]).is_err());
    assert!(explain(&[], &[outcome("source", SourceState::Collected)]).is_err());
    let mut unowned = row;
    let OperationalFact::RuntimeDistribution(point) = &mut unowned.fact else { panic!() };
    point.scope = MeasurementScope::Server;
    unowned.key = point.key().unwrap();
    assert!(explain(&[&unowned], &[outcome("source", SourceState::Partial)]).is_err());
}
