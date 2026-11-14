use super::*;
use crate::support::tests::sample;
use culprit_core::{
    Check, DistributionPopulation, RuntimeDistribution, RuntimeDistributionOrigin, RuntimeDuration,
    RuntimeHistogramBucket, RuntimeQuantile, Spread,
};

fn plan(id: &str, executions: f64, mean_time_ms: f64) -> PlanVariant {
    serde_json::from_value(serde_json::json!({
        "id": id, "executions": executions, "total_time_ms": executions * mean_time_ms,
        "mean_time_ms": mean_time_ms, "first_seen": null, "last_seen": null, "is_forced": false
    }))
    .unwrap()
}
fn query() -> QueryStat {
    let mut query = sample();
    query.id = "42".into();
    query.text = "SELECT id FROM dbo.orders WHERE customer_id=@P1".into();
    query.executions = 1_000.0;
    query.plans = vec![plan("815", 980.0, 2.0), plan("685", 20.0, 3_400.0)];
    query.total_time_ms = query.plans.iter().map(|plan| plan.total_time_ms).sum();
    query.mean_time_ms = query.total_time_ms / query.executions;
    query
}
fn snapshot(query: QueryStat) -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    let workload = snapshot.workload.as_mut().unwrap();
    workload.total_time_ms = query.total_time_ms;
    workload.queries = vec![query];
    snapshot
}
fn observation(query: QueryStat) -> Finding {
    let snapshot = snapshot(query);
    let mut findings = ParameterSensitiveCheck.run(&snapshot);
    assert_eq!(findings.len(), 1);
    findings.remove(0)
}
fn captured_distribution(p50: f64, p99: f64, bins: &[[f64; 3]]) -> RuntimeDistribution {
    RuntimeDistribution {
        source: "test.retained-runs".into(),
        origin: RuntimeDistributionOrigin::CapturedRuns,
        population: DistributionPopulation::CapturedSamples {
            sample_count: bins.iter().map(|b| b[2] as u64).sum::<u64>().to_string(),
        },
        p50: Some(RuntimeQuantile::sample(p50)),
        p95: None,
        p99: Some(RuntimeQuantile::sample(p99)),
        histogram: bins
            .iter()
            .map(|[lower, upper, samples]| RuntimeHistogramBucket {
                lower: RuntimeDuration::Milliseconds { value: *lower },
                upper: RuntimeDuration::Milliseconds { value: *upper },
                samples: (*samples as u64).to_string(),
            })
            .collect(),
        issue: None,
    }
}
fn histogram() -> QueryStat {
    let mut query = query();
    query.plans.clear();
    query.spread = Some(Spread {
        distributions: vec![captured_distribution(
            2.0,
            900.0,
            &[[0.0, 2.0, 600.0], [2.0, 4.0, 380.0], [500.0, 1_000.0, 20.0]],
        )],
        ..Default::default()
    });
    query
}

#[test]
fn overlapping_missing_and_disjoint_plan_dates_never_establish_parameter_selected_plans_or_a_mutating_fix() {
    for dates in [
        None,
        Some(("2026-10-10T00:00:00Z", "2026-10-11T00:00:00Z")),
        Some(("2026-10-01T00:00:00Z", "2026-10-02T00:00:00Z")),
    ] {
        let mut query = query();
        if let Some((first, last)) = dates {
            query.plans[0].first_seen = Some(first.into());
            query.plans[0].last_seen = Some(last.into());
            query.plans[1].first_seen = Some("2026-10-10T12:00:00Z".into());
            query.plans[1].last_seen = Some("2026-10-11T00:00:00Z".into());
        }
        let finding = observation(query);
        assert_eq!(finding.actionability, Actionability::Information);
        assert_eq!(finding.confidence, Confidence::Medium);
        assert_eq!(finding.queries[0].id, "42");
        assert!(
            matches!(finding.facts.runtime_variation, Some(FindingRuntimeVariation::PlanAverages { ref faster_plan, ref slower_plan }) if faster_plan == "815" && slower_plan == "685")
        );
        assert!(finding.fix.as_ref().unwrap().sql.is_none());
        assert!(finding.fix.as_ref().unwrap().cost.is_none());
        assert!(
            finding
                .caveats
                .iter()
                .any(|text| text.contains("does not establish harmful parameter-sensitive plan reuse"))
        );
        assert!(!finding.summary.contains("side by side"));
    }
}

#[test]
fn native_forcing_and_compilation_cost_do_not_make_the_faster_observed_plan_a_safe_remedy() {
    let mut query = query();
    query.plans[0].is_forced = true;
    query.costs = Some(culprit_core::QueryCosts { plan_ms: Some(132.0), plans: Some(4.0), ..Default::default() });
    let finding = observation(query);
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
    assert!(finding.fix.as_ref().unwrap().cost.is_none());
    assert!(finding.caveats.iter().any(|text| text.contains("marked forced")));
    assert!(!finding.summary.contains("compile on every"));
}

#[test]
fn measured_plan_and_statement_time_survive_without_counterfactual_savings_or_unweighted_averages() {
    let query = query();
    let time = query.total_time_ms;
    let finding = observation(query);
    assert_eq!(
        finding
            .metrics
            .iter()
            .find(|metric| metric.is("check.parameter_sensitive.metric.recorded_time"))
            .unwrap()
            .value,
        time
    );
    assert_eq!(
        finding
            .metrics
            .iter()
            .find(|metric| metric.is("check.parameter_sensitive.metric.slower_plan_time"))
            .unwrap()
            .value,
        68_000.0
    );
    assert_eq!(
        finding
            .metrics
            .iter()
            .find(|metric| metric.is("check.parameter_sensitive.metric.slower_plan_runs"))
            .unwrap()
            .value,
        20.0
    );
    assert!(finding.metrics.iter().all(|metric| !metric.label.contains("waste") && !metric.label.contains("saving")));
    assert_eq!(finding.impact.db_time_share, Some(1.0));
}

#[test]
fn histogram_counts_keep_their_own_denominator_without_assigning_lock_totals_to_the_tail() {
    let mut query = histogram();
    query.executions = 10_000.0;
    query.spread.as_mut().unwrap().lock_ms = Some(1_000_000.0);
    let finding = observation(query);
    assert!(matches!(finding.facts.runtime_variation, Some(FindingRuntimeVariation::RuntimeTail)));
    assert_eq!(
        finding
            .metrics
            .iter()
            .find(|metric| metric.is("check.parameter_sensitive.metric.histogram_samples"))
            .unwrap()
            .value,
        1_000.0
    );
    assert_eq!(
        finding.metrics.iter().find(|metric| metric.is("check.parameter_sensitive.metric.slow_runs")).unwrap().value,
        0.02
    );
    assert_eq!(
        finding
            .metrics
            .iter()
            .find(|metric| metric.is("check.parameter_sensitive.metric.recorded_locks"))
            .unwrap()
            .value,
        1_000_000.0
    );
    assert!(
        finding.summary.contains(&format!("20 of {} recorded histogram samples", culprit_core::format::count(1_000.0))),
        "{}",
        finding.summary
    );
    assert!(finding.caveats.iter().any(|text| text.contains("does not identify which calls waited")));
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
    let mut zero_median = histogram();
    let spread = zero_median.spread.as_mut().unwrap();
    spread.distributions = vec![captured_distribution(0.0, 900.0, &[[0.0, 0.001, 980.0], [500.0, 1_000.0, 20.0]])];
    let Shape::Tail { counted: Some(tail), .. } = tail_shape(&zero_median).unwrap() else {
        panic!("counted native tail")
    };
    assert_eq!(tail.slow_samples, 20, "zero-duration samples do not become the slow tail");
}

#[test]
fn quantiles_without_a_histogram_do_not_invent_a_tail_count_or_exact_one_in_one_hundred_runs() {
    let mut query = histogram();
    query.spread.as_mut().unwrap().distributions[0].histogram.clear();
    let finding = observation(query);
    assert!(finding.title.contains("99th percentile"));
    assert!(!finding.title.contains("1 in 100"));
    assert!(finding.metrics.iter().all(|metric| !metric.is("check.parameter_sensitive.metric.slow_runs")));
    assert!(finding.metrics.iter().all(|metric| !metric.is("check.parameter_sensitive.metric.histogram_samples")));
}

#[test]
fn missing_median_and_malformed_native_bins_do_not_manufacture_a_distribution() {
    let mut query = histogram();
    query.spread.as_mut().unwrap().distributions[0].p50 = None;
    query.spread.as_mut().unwrap().distributions[0].p95 = Some(RuntimeQuantile::sample(2.0));
    assert!(tail_shape(&query).is_none(), "p95 or mean is not a median");
    for malformed in ["negative-bound", "negative-count", "reversed", "infinite"] {
        let mut query = histogram();
        let bucket = &mut query.spread.as_mut().unwrap().distributions[0].histogram[2];
        match malformed {
            "negative-bound" => bucket.lower = RuntimeDuration::Milliseconds { value: -1.0 },
            "negative-count" => bucket.samples = "-1".into(),
            "reversed" => bucket.upper = RuntimeDuration::Milliseconds { value: 4.0 },
            "infinite" => bucket.upper = RuntimeDuration::Milliseconds { value: f64::INFINITY },
            _ => unreachable!(),
        }
        assert!(tail_shape(&query).is_none());
    }
    let mut query = histogram();
    query.spread.as_mut().unwrap().distributions[0] =
        captured_distribution(2.0, 4.0, &[[0.0, 2.0, 600.0], [2.0, 4.0, 380.0], [500.0, 1_000.0, 2.0]]);
    assert!(tail_shape(&query).is_none(), "the counted tail needs several observations");
}

#[test]
fn invalid_plan_counters_and_duplicate_ids_do_not_authorize_plan_comparison() {
    for malformed in ["mean", "count", "total", "id", "foreign-population"] {
        let mut query = query();
        match malformed {
            "mean" => query.plans[0].mean_time_ms = f64::NAN,
            "count" => query.plans[0].executions = -1.0,
            "total" => query.plans[0].total_time_ms = f64::INFINITY,
            "id" => query.plans[0].id = query.plans[1].id.clone(),
            "foreign-population" => query.plans[0].executions = query.executions + 1.0,
            _ => unreachable!(),
        }
        assert!(plans_shape(&query).is_none(), "{malformed}");
    }
}

#[test]
fn recorded_compilation_context_preserves_null_values_without_pretending_they_were_runtime_inputs() {
    let mut query = query();
    query.plans[0].compiled = Some("@P1 = NULL, @P2 = (42)".into());
    assert_eq!(compiled_context(&query.plans[0], &query).as_deref(), Some("@P1 = NULL, @P2 = (42)"));
    let finding = observation(query);
    assert!(
        finding.caveats.iter().any(|text| text.contains("does not establish the inputs of its recorded executions"))
    );
}

#[test]
fn the_same_typed_distribution_remains_an_observation_on_every_engine() {
    for engine in Engine::ALL {
        let mut snapshot = snapshot(histogram());
        snapshot.server.engine = engine;
        snapshot.pressure.get_or_insert_default().uptime_hours = Some(0.5);
        let findings = ParameterSensitiveCheck.run(&snapshot);
        assert_eq!(findings.len(), 1, "{engine:?}");
        assert_eq!(findings[0].actionability, Actionability::Information);
        assert!(findings[0].fix.as_ref().unwrap().sql.is_none());
    }
}

#[test]
fn native_variant_routing_is_context_and_does_not_establish_fault_or_mingle_parent_runtime() {
    use culprit_core::telemetry::{NativePlanIdentity, NativeQueryIdentity};
    let parent = NativeQueryIdentity { query_id: "7".into(), qualifiers: Default::default() };
    let mut query = query();
    query.variant_family = Some(culprit_core::QueryVariantFamily {
        variant: NativeQueryIdentity { query_id: query.id.clone(), qualifiers: Default::default() },
        parent: parent.clone(),
        dispatcher: NativePlanIdentity { plan_id: "1".into(), query: parent, qualifiers: Default::default() },
    });
    assert!(query.variant_family.as_ref().unwrap().is_valid());
    let finding = observation(query.clone());
    assert!(finding.caveats.iter().any(|text| text.contains("native routed variant")));
    assert_eq!(finding.actionability, Actionability::Information);
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
    query.plans[1].mean_time_ms = 3.0;
    query.plans[1].total_time_ms = query.plans[1].executions * 3.0;
    assert!(plans_shape(&query).is_none(), "a healthy native variant is not itself a fault");
}

#[test]
fn histogram_bounds_do_not_become_exact_percentiles_or_borrow_summary_extrema_and_counts() {
    let mut query = histogram();
    query.executions = 1.0;
    query.total_time_ms = 1.0;
    query.mean_time_ms = 1.0;
    query.max_time_ms = Some(1.0);
    let bins = query.spread.as_ref().unwrap().distributions[0].histogram.clone();
    query.spread.as_mut().unwrap().distributions = vec![RuntimeDistribution::native_histogram("native.buckets", bins)];
    let finding = observation(query);
    assert_eq!(finding.actionability, Actionability::Information);
    assert!(finding.caveats.iter().any(|value| value.contains("observed independently")));
    assert_eq!(
        finding.metrics.iter().find(|metric| metric.is("check.parameter_sensitive.metric.p99_upper")).unwrap().value,
        1_000.0
    );
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
}

#[test]
fn quantiles_from_separate_reads_or_without_a_lower_bound_never_prove_a_tail() {
    let mut query = histogram();
    let summary = |source: &str, p50, p99| RuntimeDistribution {
        source: source.into(),
        origin: RuntimeDistributionOrigin::StatementSummary,
        population: DistributionPopulation::NativeCounters { sample_count: Some("1000".into()) },
        p50,
        p95: None,
        p99,
        histogram: Vec::new(),
        issue: None,
    };
    let upper =
        |value| RuntimeQuantile::HistogramUpperBound { lower: None, upper: RuntimeDuration::Milliseconds { value } };
    query.spread.as_mut().unwrap().distributions =
        vec![summary("first-read", Some(upper(2.0)), None), summary("later-read", None, Some(upper(900.0)))];
    assert!(tail_shape(&query).is_none());
    query.spread.as_mut().unwrap().distributions = vec![summary("one-read", Some(upper(2.0)), Some(upper(900.0)))];
    assert!(tail_shape(&query).is_none(), "a high estimate supplies no p99 lower bound");
}

#[test]
fn invalid_or_unavailable_independent_reads_do_not_hide_a_good_captured_population() {
    let mut query = histogram();
    query.spread.as_mut().unwrap().distributions.insert(
        0,
        RuntimeDistribution::unavailable(
            "native.summary",
            RuntimeDistributionOrigin::StatementSummary,
            culprit_core::RuntimeDistributionIssue::ReadUnavailable,
        ),
    );
    assert!(tail_shape(&query).is_some());
    query.spread.as_mut().unwrap().distributions[1].issue =
        Some(culprit_core::RuntimeDistributionIssue::InconsistentPopulation);
    assert!(tail_shape(&query).is_none());
}
