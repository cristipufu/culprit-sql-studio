//! Recorded plan-population or runtime-distribution differences are investigation evidence, not proof that
//! parameter values selected a plan or that another execution population could use the faster plan.
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use crate::queries::apply_impact;
use crate::support::*;
use culprit_core::{
    Actionability, Category, CheckInfo, Confidence, Engine, Finding, FindingRuntimeVariation, Fix, Metric, PlanVariant,
    QueryStat, RuntimeDistribution, RuntimeQuantile, Snapshot, SubjectKind, Unit, format, t,
};
use std::collections::BTreeSet;

const MIN_RUNS: f64 = 50.0;
const MIN_PLAN_RUNS: f64 = 10.0;
const MIN_PLAN_SHARE: f64 = 0.01;
const MIN_FACTOR: f64 = 5.0;
const MIN_GAP_MS: f64 = 5.0;
const MIN_TAIL_FACTOR: f64 = 10.0;
const MIN_TAIL_BASE_MS: f64 = 0.05;
const MIN_TAIL_MS: f64 = 200.0;
const MAX_FINDINGS: usize = 6;
pub(crate) const MIN_TAIL_RUNS: f64 = 3.0;

pub fn parameter_sensitive_info() -> CheckInfo {
    CheckInfo {
        id: "parameter-sensitive".into(),
        title: t!("check.parameter_sensitive.info.title"),
        category: Category::Queries,
        engines: Engine::ALL.to_vec(),
        requires: vec![],
        description: t!("check.parameter_sensitive.info.description"),
    }
}
check!(
    ParameterSensitiveCheck: parameter_sensitive_info,
    decide = parameter_sensitive_cases,
    render = parameter_sensitive_finding,
    case<'s> = ParameterSensitive<'s>,
    evidence = &[Evidence::Query(QueryMeasure::ParameterSpread)],
);

#[derive(Debug)]
enum Shape<'a> {
    Plans { faster: &'a PlanVariant, slower: &'a PlanVariant },
    Tail { distribution: &'a RuntimeDistribution, counted: Option<HistogramTail> },
}
#[derive(Debug)]
struct HistogramTail {
    samples: u64,
    slow_samples: u64,
    lower_bound_ms: f64,
}

fn nonnegative(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}
fn valid_statement(query: &QueryStat) -> bool {
    nonnegative(query.executions) && query.executions >= MIN_RUNS && nonnegative(query.total_time_ms)
}
fn valid_plans(query: &QueryStat) -> bool {
    let mut ids = BTreeSet::new();
    query.workload_plans().iter().all(|plan| {
        !plan.id.is_empty()
            && ids.insert(&plan.id)
            && nonnegative(plan.executions)
            && plan.executions <= query.executions
            && nonnegative(plan.mean_time_ms)
            && nonnegative(plan.total_time_ms)
    })
}
fn plans_shape(query: &QueryStat) -> Option<Shape<'_>> {
    if !valid_statement(query) || !valid_plans(query) {
        return None;
    }
    let eligible = || {
        query
            .workload_plans()
            .iter()
            .filter(|plan| plan.executions >= MIN_PLAN_RUNS && plan.executions >= query.executions * MIN_PLAN_SHARE)
    };
    let faster = eligible().min_by(|left, right| left.mean_time_ms.total_cmp(&right.mean_time_ms))?;
    let slower = eligible().max_by(|left, right| left.mean_time_ms.total_cmp(&right.mean_time_ms))?;
    (faster.id != slower.id
        && slower.mean_time_ms >= faster.mean_time_ms * MIN_FACTOR
        && slower.mean_time_ms - faster.mean_time_ms >= MIN_GAP_MS)
        .then_some(Shape::Plans { faster, slower })
}

fn histogram_tail(distribution: &RuntimeDistribution, lower_bound_ms: f64) -> Option<HistogramTail> {
    let samples = distribution.population.sample_count()?;
    let mut slow_samples = 0u64;
    for bucket in &distribution.histogram {
        if bucket.lower.milliseconds()? >= lower_bound_ms {
            slow_samples = slow_samples.checked_add(bucket.samples.parse::<u64>().ok()?)?;
        }
    }
    (samples as f64 >= MIN_RUNS && slow_samples as f64 >= MIN_TAIL_RUNS).then_some(HistogramTail {
        samples,
        slow_samples,
        lower_bound_ms,
    })
}
fn tail_shape(query: &QueryStat) -> Option<Shape<'_>> {
    if !nonnegative(query.total_time_ms) {
        return None;
    }
    query.spread.as_ref()?.bounded_distributions()?.iter().filter(|observed| observed.is_usable()).find_map(
        |distribution| {
            if distribution.population.sample_count()? < MIN_RUNS as u64 {
                return None;
            }
            let (median_upper, p99_lower) =
                (distribution.p50.as_ref()?.upper_ms()?, distribution.p99.as_ref()?.lower_ms()?);
            let lower_bound_ms = median_upper.max(MIN_TAIL_BASE_MS) * MIN_TAIL_FACTOR;
            if p99_lower < lower_bound_ms || p99_lower < MIN_TAIL_MS {
                return None;
            }
            let counted = if distribution.histogram.is_empty() {
                None
            } else {
                Some(histogram_tail(distribution, lower_bound_ms)?)
            };
            Some(Shape::Tail { distribution, counted })
        },
    )
}

pub struct ParameterSensitive<'s> {
    query: &'s QueryStat,
    shape: Shape<'s>,
    impact: Option<QueryImpact>,
    score: f64,
}
fn parameter_sensitive_cases(snapshot: &Snapshot) -> Vec<ParameterSensitive<'_>> {
    let Some(workload) = &snapshot.workload else { return Vec::new() };
    let mut cases: Vec<_> = app_queries(snapshot)
        .filter_map(|query| {
            let shape = plans_shape(query).or_else(|| tail_shape(query))?;
            let share = workload.statement_share(query);
            (share.is_finite() && share >= 0.01).then(|| ParameterSensitive {
                query,
                shape,
                impact: query_impact(snapshot, &[query]),
                score: score_for_share(share),
            })
        })
        .collect();
    cases.sort_by(|left, right| right.score.total_cmp(&left.score));
    cases.truncate(MAX_FINDINGS);
    cases
}

fn compiled_context(plan: &PlanVariant, query: &QueryStat) -> Option<String> {
    plan.compiled.clone().or_else(|| {
        (query.captured_plan_variant()?.id == plan.id)
            .then(|| query.plan.as_ref()?.facts().compiled_parameters.clone())?
    })
}
fn append_compilation_context(finding: &mut Finding, plan: &PlanVariant, query: &QueryStat) {
    if let Some(context) = compiled_context(plan, query).filter(|context| !context.trim().is_empty()) {
        finding.caveats.push(t!("check.parameter_sensitive.compilation_context", plan = &plan.id, context = context));
    }
}
fn tail_source(snapshot: &Snapshot, distribution: &RuntimeDistribution) -> String {
    if matches!(distribution.population, culprit_core::DistributionPopulation::NativeCounters { .. }) {
        return t!("check.parameter_sensitive.source.native", source = &distribution.source);
    }
    if snapshot.workload.as_ref().is_some_and(|workload| workload.kind() == Some(culprit_core::WorkloadKind::SlowLog)) {
        let threshold = snapshot
            .setting("long_query_time")
            .and_then(|setting| setting.value.trim().parse::<f64>().ok())
            .filter(|seconds| nonnegative(*seconds))
            .map(|seconds| seconds * 1000.0)
            .filter(|milliseconds| milliseconds.is_finite());
        return match threshold {
            Some(milliseconds) => {
                t!("check.parameter_sensitive.source.log_kept_over", threshold = format::duration(milliseconds))
            }
            None => t!("check.parameter_sensitive.source.log_kept"),
        };
    }
    t!("check.parameter_sensitive.source.samples")
}
fn plan_observation(finding: &mut Finding, query: &QueryStat, faster: &PlanVariant, slower: &PlanVariant) {
    finding.title = t!(
        "check.parameter_sensitive.title.plans",
        query = query_handle(query),
        fast = format::duration(faster.mean_time_ms),
        slow = format::duration(slower.mean_time_ms)
    );
    finding.summary = t!(
        "check.parameter_sensitive.summary.plans",
        fast = &faster.id,
        fast_count = format::count(faster.executions),
        fast_ms = format::duration(faster.mean_time_ms),
        slow = &slower.id,
        slow_count = format::count(slower.executions),
        slow_ms = format::duration(slower.mean_time_ms)
    );
    finding.facts.runtime_variation =
        Some(FindingRuntimeVariation::PlanAverages { faster_plan: faster.id.clone(), slower_plan: slower.id.clone() });
    finding.metrics.extend([
        Metric::said(culprit_core::key!("check.parameter_sensitive.metric.faster_plan"), faster.mean_time_ms, Unit::Ms),
        Metric::said(culprit_core::key!("check.parameter_sensitive.metric.slow_plan"), slower.mean_time_ms, Unit::Ms),
        Metric::said(
            culprit_core::key!("check.parameter_sensitive.metric.slower_plan_time"),
            slower.total_time_ms,
            Unit::Ms,
        ),
        Metric::said(
            culprit_core::key!("check.parameter_sensitive.metric.slower_plan_runs"),
            slower.executions,
            Unit::Count,
        ),
    ]);
    for plan in [faster, slower] {
        append_compilation_context(finding, plan, query);
    }
    if query.workload_plans().iter().any(|plan| plan.is_forced) {
        finding.caveats.push(t!("check.parameter_sensitive.caveat.forcing"));
    }
}
fn tail_observation(
    finding: &mut Finding,
    snapshot: &Snapshot,
    query: &QueryStat,
    distribution: &RuntimeDistribution,
    counted: Option<HistogramTail>,
) {
    let median = distribution.p50.as_ref().expect("the case requires a median");
    let p99 = distribution.p99.as_ref().expect("the case requires a p99");
    let median_ms = median.upper_ms().expect("the case validates durations");
    let p99_ms = p99.upper_ms().expect("the case validates durations");
    let exact_samples =
        matches!(median, RuntimeQuantile::SampleValue { .. }) && matches!(p99, RuntimeQuantile::SampleValue { .. });
    if exact_samples {
        finding.title = t!(
            "check.parameter_sensitive.title.tail_observed",
            query = query_handle(query),
            median = format::duration(median_ms),
            p99 = format::duration(p99_ms)
        );
        finding.summary = t!(
            "check.parameter_sensitive.summary.tail_observed",
            source = tail_source(snapshot, distribution),
            median = format::duration(median_ms),
            p99 = format::duration(p99_ms)
        );
    } else {
        let lower = format::duration(p99.lower_ms().expect("the case requires a lower bound"));
        finding.title = t!(
            "check.parameter_sensitive.title.tail_bounds",
            query = query_handle(query),
            median = format::duration(median_ms),
            lower = &lower,
            upper = format::duration(p99_ms)
        );
        finding.summary = t!(
            "check.parameter_sensitive.summary.tail_bounds",
            source = tail_source(snapshot, distribution),
            median = format::duration(median_ms),
            lower = lower,
            upper = format::duration(p99_ms)
        );
    }
    finding.facts.runtime_variation = Some(FindingRuntimeVariation::RuntimeTail);
    finding.metrics.extend([
        Metric::said(
            if exact_samples {
                culprit_core::key!("check.parameter_sensitive.metric.typical_run")
            } else {
                culprit_core::key!("check.parameter_sensitive.metric.median_upper")
            },
            median_ms,
            Unit::Ms,
        ),
        Metric::said(
            if exact_samples {
                culprit_core::key!("check.parameter_sensitive.metric.p99")
            } else {
                culprit_core::key!("check.parameter_sensitive.metric.p99_upper")
            },
            p99_ms,
            Unit::Ms,
        ),
    ]);
    if let Some(counted) = counted {
        finding.summary.push(' ');
        finding.summary.push_str(&t!(
            "check.parameter_sensitive.summary.histogram_samples",
            slow = format::count(counted.slow_samples as f64),
            samples = format::count(counted.samples as f64),
            threshold = format::duration(counted.lower_bound_ms)
        ));
        finding.metrics.extend([
            count_metric(culprit_core::key!("check.parameter_sensitive.metric.histogram_samples"), counted.samples),
            Metric::said(
                culprit_core::key!("check.parameter_sensitive.metric.slow_runs"),
                counted.slow_samples as f64 / counted.samples as f64,
                Unit::Ratio,
            ),
        ]);
    }
    if matches!(distribution.population, culprit_core::DistributionPopulation::NativeCounters { .. }) {
        finding.caveats.push(t!("check.parameter_sensitive.caveat.native_population"));
    }
    if let Some(milliseconds) =
        query.spread.as_ref().and_then(|spread| spread.lock_ms).filter(|value| nonnegative(*value) && *value > 0.0)
    {
        finding.metrics.push(Metric::said(
            culprit_core::key!("check.parameter_sensitive.metric.recorded_locks"),
            milliseconds,
            Unit::Ms,
        ));
        finding.caveats.push(t!("check.parameter_sensitive.caveat.locks"));
    }
}
fn parameter_sensitive_finding(snapshot: &Snapshot, case: ParameterSensitive<'_>) -> Finding {
    let query = case.query;
    let mut finding =
        finding("parameter-sensitive", &query.id, Category::Queries, subject(SubjectKind::Query, &query.id));
    if let Some(impact) = &case.impact {
        apply_impact(snapshot, &mut finding, impact);
    }
    finding.severity = culprit_core::Severity::Low;
    finding.actionability = Actionability::Information;
    finding.confidence = Confidence::Medium;
    finding.impact.score = case.score;
    finding.impact.set_label(t!("check.parameter_sensitive.impact.recorded"));
    finding.metrics.push(Metric::said(
        culprit_core::key!("check.parameter_sensitive.metric.recorded_time"),
        query.total_time_ms,
        Unit::Ms,
    ));
    match case.shape {
        Shape::Plans { faster, slower } => plan_observation(&mut finding, query, faster, slower),
        Shape::Tail { distribution, counted } => tail_observation(&mut finding, snapshot, query, distribution, counted),
    }
    finding.caveats.push(t!("check.parameter_sensitive.caveat.alternatives"));
    if query.variant_family.is_some() {
        finding.caveats.push(t!("check.parameter_sensitive.caveat.native_routing"));
    }
    if let Some(basis) = totals_basis(snapshot, query) {
        finding.caveats.push(t!("check.parameter_sensitive.caveat.population", basis = basis));
    }
    finding.fix = Some(runtime_variation_fix());
    finding.queries = query_refs(&[query]);
    finding.verify = vec![
        t!("check.parameter_sensitive.verify.compare"),
        t!(
            "check.parameter_sensitive.verify.rescan",
            queries = culprit_core::t!("ui.common.view.queries"),
            spread = culprit_core::t!("ui.runtime_spread.title")
        ),
    ];
    finding
}

fn count_metric(key: &str, count: u64) -> Metric {
    if count <= (1u64 << 53) {
        Metric::said(key, count as f64, Unit::Count)
    } else {
        Metric::said_text(key, count.to_string())
    }
}

/// No cause-specific mutation is authorized by a plan mean, variance or tail alone.
pub(crate) fn runtime_variation_fix() -> Fix {
    Fix { summary: t!("check.parameter_sensitive.fix.investigate"), sql: None, cost: None }
}

#[cfg(test)]
mod tests;
