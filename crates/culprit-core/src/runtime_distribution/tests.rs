use super::*;

fn ps(value: u64) -> RuntimeDuration {
    RuntimeDuration::Picoseconds { value: value.to_string() }
}

#[test]
fn signed_zero_is_equal_duration_and_cannot_form_a_positive_width_bucket() {
    for (lower, upper) in [(-0.0, 0.0), (0.0, -0.0)] {
        let lower = RuntimeDuration::Milliseconds { value: lower };
        let upper = RuntimeDuration::Milliseconds { value: upper };
        let bucket = RuntimeHistogramBucket { lower: lower.clone(), upper: upper.clone(), samples: "100".into() };
        assert!(!bucket.is_valid());
        assert!(!RuntimeQuantile::HistogramUpperBound { lower: Some(lower), upper }.is_valid());
        let observed = RuntimeDistribution::native_histogram("native.histogram", vec![bucket]);
        assert!(!observed.is_usable());
        assert_eq!(observed.issue, Some(RuntimeDistributionIssue::InconsistentPopulation));
    }
}

#[test]
fn signed_zero_sample_quantiles_keep_raw_values_without_becoming_out_of_order() {
    let observed = RuntimeDistribution {
        source: "retained.samples".into(),
        origin: RuntimeDistributionOrigin::CapturedRuns,
        population: DistributionPopulation::CapturedSamples { sample_count: "100".into() },
        p50: Some(RuntimeQuantile::sample(0.0)),
        p95: Some(RuntimeQuantile::sample(-0.0)),
        p99: Some(RuntimeQuantile::sample(0.0)),
        histogram: vec![],
        issue: None,
    };
    assert!(observed.is_usable());
    let restored: RuntimeDistribution = serde_json::from_slice(&serde_json::to_vec(&observed).unwrap()).unwrap();
    let Some(RuntimeQuantile::SampleValue { value: RuntimeDuration::Milliseconds { value } }) = restored.p95 else {
        panic!()
    };
    assert_eq!(value.to_bits(), (-0.0f64).to_bits());
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert!(!RuntimeQuantile::sample(invalid).is_valid());
    }
}
fn bucket(lower: u64, upper: u64, samples: u64) -> RuntimeHistogramBucket {
    RuntimeHistogramBucket { lower: ps(lower), upper: ps(upper), samples: samples.to_string() }
}
fn histogram() -> RuntimeDistribution {
    RuntimeDistribution::native_histogram(
        "native.latency_histogram",
        vec![bucket(0, 2_000_000_000, 980), bucket(500_000_000_000, 1_000_000_000_000, 20)],
    )
}

#[test]
fn native_bounds_and_counts_survive_exactly_above_float_integer_precision() {
    let observed = RuntimeDistribution::native_histogram(
        "native.latency_histogram",
        vec![bucket(0, 9_007_199_254_740_993, u64::MAX - 2), bucket(9_007_199_254_740_993, u64::MAX, 2)],
    );
    assert!(observed.is_usable());
    assert_eq!(observed.population.sample_count(), Some(u64::MAX));
    let restored: RuntimeDistribution = serde_json::from_slice(&serde_json::to_vec(&observed).unwrap()).unwrap();
    assert_eq!(restored, observed);
    assert_eq!(restored.histogram[0].upper, ps(9_007_199_254_740_993));
    assert_eq!(restored.histogram[1].upper, ps(u64::MAX));
    assert!(restored.p99.as_ref().unwrap().upper_ms().unwrap().is_finite());
}

#[test]
fn histogram_quantiles_keep_both_original_bounds_and_their_own_integer_rank() {
    let observed = histogram();
    assert!(observed.is_usable());
    assert_eq!(observed.population.sample_count(), Some(1_000));
    let p99 = observed.p99.as_ref().unwrap();
    assert_eq!((p99.lower_ms(), p99.upper_ms()), (Some(500.0), Some(1_000.0)));
    let mut wrong = observed.clone();
    wrong.p99 = Some(RuntimeQuantile::HistogramUpperBound { lower: Some(ps(1)), upper: ps(2) });
    assert!(!wrong.is_valid(), "a percentile cannot borrow another bucket or population");
}

#[test]
fn captured_sample_percentiles_never_claim_all_database_executions() {
    let observed = RuntimeDistribution {
        source: "retained.log".into(),
        origin: RuntimeDistributionOrigin::CapturedRuns,
        population: DistributionPopulation::CapturedSamples { sample_count: "80".into() },
        p50: Some(RuntimeQuantile::sample(150.0)),
        p95: None,
        p99: Some(RuntimeQuantile::sample(4_000.0)),
        histogram: Vec::new(),
        issue: None,
    };
    assert!(observed.is_usable());
    assert!(matches!(observed.population, DistributionPopulation::CapturedSamples { .. }));
    let mut wrong = observed;
    wrong.population = DistributionPopulation::NativeCounters { sample_count: Some("80".into()) };
    assert!(!wrong.is_valid());
}

#[test]
fn summary_bounds_need_no_histogram_or_guessed_sample_count_or_lower_bound() {
    let observed = RuntimeDistribution {
        source: "native.summary".into(),
        origin: RuntimeDistributionOrigin::StatementSummary,
        population: DistributionPopulation::NativeCounters { sample_count: None },
        p50: None,
        p95: None,
        p99: Some(RuntimeQuantile::HistogramUpperBound { lower: None, upper: ps(900_000_000_000) }),
        histogram: Vec::new(),
        issue: None,
    };
    assert!(observed.is_usable());
    assert_eq!(observed.population.sample_count(), None);
    assert_eq!(observed.p99.as_ref().unwrap().lower_ms(), None);
}

#[test]
fn malformed_and_contradictory_native_populations_cannot_be_used() {
    for shape in ["order", "overlap", "count", "overflow", "unit", "reverse", "count-width", "negative"] {
        let mut observed = histogram();
        match shape {
            "order" => observed.p50 = observed.p99.clone(),
            "overlap" => observed.histogram[1].lower = ps(1),
            "count" => {
                observed.population = DistributionPopulation::NativeCounters { sample_count: Some("999".into()) }
            }
            "overflow" => observed.histogram[0].samples = u64::MAX.to_string(),
            "unit" => observed.histogram[0].upper = RuntimeDuration::Milliseconds { value: 2.0 },
            "reverse" => observed.histogram[1].upper = ps(1),
            "count-width" => observed.histogram[0].samples = "18446744073709551616".into(),
            "negative" => observed.histogram[0].samples = "-1".into(),
            _ => unreachable!(),
        }
        assert!(!observed.is_usable(), "{shape}");
    }
    for invalid in [f64::NAN, f64::INFINITY, -1.0] {
        assert!(!RuntimeQuantile::sample(invalid).is_valid());
    }
    for invalid in ["01", "+1", " 1", "1.0", "-1", "18446744073709551616"] {
        assert_eq!(RuntimeDuration::Picoseconds { value: invalid.into() }.milliseconds(), None);
    }
}

#[test]
fn missing_or_invalid_observations_do_not_erase_a_good_independent_population() {
    let good = histogram();
    let unavailable = RuntimeDistribution::unavailable(
        "native.summary",
        RuntimeDistributionOrigin::StatementSummary,
        RuntimeDistributionIssue::ReadUnavailable,
    );
    assert!(unavailable.is_valid());
    assert!(!unavailable.is_usable());
    let spread = crate::Spread { distributions: vec![unavailable, good.clone()], ..Default::default() };
    let usable: Vec<_> = spread.bounded_distributions().unwrap().iter().filter(|value| value.is_usable()).collect();
    assert_eq!(usable, vec![&good]);
}

#[test]
fn native_bucket_and_observation_limits_are_enforced_before_analysis() {
    let mut observed = histogram();
    observed.histogram = vec![bucket(0, 1, 1); MAX_RUNTIME_HISTOGRAM_BUCKETS + 1];
    assert!(!observed.is_valid());
    let spread =
        crate::Spread { distributions: vec![histogram(); MAX_RUNTIME_DISTRIBUTIONS + 1], ..Default::default() };
    assert!(spread.bounded_distributions().is_none());
    let mut source = histogram();
    source.source = "x".repeat(257);
    assert!(!source.is_valid());
}

#[test]
fn a_native_empty_histogram_contains_no_quantiles_or_complete_history_claim() {
    let observed = RuntimeDistribution::native_histogram("native.latency_histogram", Vec::new());
    assert!(observed.is_valid());
    assert_eq!(observed.population.sample_count(), Some(0));
    assert!(observed.p50.is_none() && observed.p95.is_none() && observed.p99.is_none());
}

#[test]
fn exact_sample_quantiles_must_fit_their_own_histogram_rank_when_both_are_present() {
    let ms = |value| RuntimeDuration::Milliseconds { value };
    let mut observed = RuntimeDistribution {
        source: "retained.samples".into(),
        origin: RuntimeDistributionOrigin::CapturedRuns,
        population: DistributionPopulation::CapturedSamples { sample_count: "1000".into() },
        p50: Some(RuntimeQuantile::sample(1.0)),
        p95: Some(RuntimeQuantile::sample(1.0)),
        p99: Some(RuntimeQuantile::sample(900.0)),
        histogram: vec![
            RuntimeHistogramBucket { lower: ms(0.0), upper: ms(2.0), samples: "980".into() },
            RuntimeHistogramBucket { lower: ms(500.0), upper: ms(1000.0), samples: "20".into() },
        ],
        issue: None,
    };
    assert!(observed.is_valid());
    observed.p99 = Some(RuntimeQuantile::sample(4_000.0));
    assert!(!observed.is_valid(), "an exact quantile cannot lie outside its captured histogram");
    observed.p99 = Some(RuntimeQuantile::sample(1.0));
    assert!(!observed.is_valid(), "an exact quantile cannot borrow a faster rank's bucket");
}
