//! Native fraction inputs are aggregate context, never a pressure grade, interval rate or query culprit.
use culprit_core::{
    CounterRatioEntry, CounterRatioReport, Metric, Result, SourceOutcome, SourceState, format, t,
    telemetry::{CounterRatioIssue, CounterRatioKind, NativeCounterRatio, NativeCounterSet},
};

pub const MAX_COUNTER_RATIO_ENTRIES: usize = 16;

pub fn explain(
    samples: &[NativeCounterSet],
    source: Option<&SourceOutcome>,
    read_complete: bool,
) -> Result<CounterRatioReport> {
    let samples = samples.iter().collect::<Vec<_>>();
    explain_borrowed(&samples, source, read_complete)
}

pub(super) fn explain_borrowed(
    samples: &[&NativeCounterSet],
    source: Option<&SourceOutcome>,
    read_complete: bool,
) -> Result<CounterRatioReport> {
    if samples.len() > culprit_core::telemetry::CollectionBudget::upper_limits().max_snapshot_rows as usize
        || samples.iter().any(|sample| !sample.is_valid())
        || !super::source_coverage::consistent_source(
            source,
            samples.iter().map(|_| culprit_core::source_id::PERFORMANCE_COUNTERS),
        )
    {
        return Err(culprit_core::Error::invalid(t!("view.counter_ratios.invalid")));
    }
    let mut keys = std::collections::BTreeSet::new();
    for sample in samples {
        if !keys.insert(sample.key()?) {
            return Err(culprit_core::Error::invalid(t!("view.counter_ratios.invalid")));
        }
    }
    let observed = samples.iter().map(|sample| sample.ratios.len()).sum::<usize>();
    let mut entries = Vec::new();
    for sample in samples {
        for ratio in &sample.ratios {
            if entries.len() < MAX_COUNTER_RATIO_ENTRIES {
                entries.push(entry(sample, ratio)?);
            }
        }
    }
    let limited = observed > entries.len();
    let mut limitations = vec![
        t!("view.counter_ratios.limit.scope"),
        t!("view.counter_ratios.limit.aggregate"),
        t!("view.counter_ratios.limit.cause"),
    ];
    if !read_complete {
        limitations.push(t!("view.counter_ratios.limit.read"));
    }
    if limited {
        limitations.push(t!("view.counter_ratios.limit.presentation"));
    }
    Ok(CounterRatioReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        read_complete,
        declared_ratios: observed as u32,
        presented_ratios: entries.len() as u32,
        entries_limited: limited,
        title: t!("view.counter_ratios.title"),
        summary: if !read_complete
            || source.is_some_and(|source| matches!(source.state, SourceState::Partial | SourceState::Unavailable))
        {
            t!("view.counter_ratios.summary.partial")
        } else if entries.is_empty() {
            t!("view.counter_ratios.summary.absent")
        } else {
            t!("view.counter_ratios.summary.observed")
        },
        limitations,
        entries,
    })
}

fn entry(sample: &NativeCounterSet, ratio: &NativeCounterRatio) -> Result<CounterRatioEntry> {
    let issues = ratio.issues(sample);
    let input = |key: Option<&str>| key.and_then(|key| sample.counters.iter().find(|counter| counter.key == key));
    let gap = |key: Option<&str>| key.and_then(|key| sample.ratio_gaps.iter().find(|gap| gap.key == key));
    let raw = |key: Option<&str>| {
        input(key)
            .and_then(|counter| counter.value.integer())
            .map(|value| value.to_string())
            .or_else(|| gap(key).map(|gap| gap.raw_value.clone()))
    };
    let mut metrics = vec![
        Metric::text(
            t!("view.counter_ratios.metric.numerator"),
            raw(ratio.numerator.as_deref()).map(format::digits).unwrap_or_else(|| t!("view.counter_ratios.unknown")),
        )
        .keyed("view.counter_ratios.metric.numerator"),
        Metric::text(
            t!("view.counter_ratios.metric.base"),
            raw(ratio.base.as_deref()).map(format::digits).unwrap_or_else(|| t!("view.counter_ratios.unknown")),
        )
        .keyed("view.counter_ratios.metric.base"),
    ];
    if let Some(percent) = ratio.percent(sample) {
        metrics.push(
            Metric::text(
                t!("view.counter_ratios.metric.percent"),
                t!("view.counter_ratios.percent", value = format::digits(percent)),
            )
            .keyed("view.counter_ratios.metric.percent"),
        );
    }
    let descriptor = input(ratio.numerator.as_deref()).or_else(|| input(ratio.base.as_deref()));
    let rejected = gap(ratio.numerator.as_deref()).or_else(|| gap(ratio.base.as_deref()));
    let (native_object, native_instance) = if let Some(counter) = descriptor {
        (Some(counter.native_object.clone()), counter.native_instance.clone())
    } else if let Some(gap) = rejected {
        (Some(gap.native_object.clone()), gap.native_instance.clone())
    } else {
        (None, None)
    };
    let mut words = vec![match ratio.kind {
        CounterRatioKind::LocalSsdCacheHit => t!("view.counter_ratios.local_ssd.explanation"),
        CounterRatioKind::PlanCacheHit => t!("view.performance_counters.plan_cache_ratio.explanation"),
        CounterRatioKind::BufferCacheHit => t!("view.performance_counters.buffer_cache_ratio.explanation"),
    }];
    words.extend(issues.iter().map(|issue| issue_words(*issue)));
    Ok(CounterRatioEntry {
        observation_key: sample.key()?,
        kind: ratio.kind,
        scope: sample.scope,
        observed_at: ratio.observed_at.clone(),
        native_object,
        native_instance,
        issues,
        title: match ratio.kind {
            CounterRatioKind::LocalSsdCacheHit => t!("view.counter_ratios.local_ssd.title"),
            CounterRatioKind::PlanCacheHit => t!("view.performance_counters.plan_cache_ratio.title"),
            CounterRatioKind::BufferCacheHit => t!("view.performance_counters.buffer_cache_ratio.title"),
        },
        explanation: words.join(" "),
        metrics,
    })
}

fn issue_words(issue: CounterRatioIssue) -> String {
    use CounterRatioIssue::*;
    match issue {
        UnknownProfile => t!("view.counter_ratios.issue.unknown_profile"),
        UnsupportedProfile => t!("view.counter_ratios.issue.unsupported_profile"),
        ReadClockUnavailable => t!("view.counter_ratios.issue.clock"),
        MissingNumerator => t!("view.counter_ratios.issue.missing_numerator"),
        MissingBase => t!("view.counter_ratios.issue.missing_base"),
        InvalidNumerator => t!("view.counter_ratios.issue.invalid_numerator"),
        InvalidBase => t!("view.counter_ratios.issue.invalid_base"),
        UnsupportedCounterType => t!("view.counter_ratios.issue.counter_type"),
        IncompatibleInputs => t!("view.counter_ratios.issue.inputs"),
        ZeroBase => t!("view.counter_ratios.issue.zero_base"),
        OutsideSubset => t!("view.counter_ratios.issue.outside_subset"),
    }
}

#[cfg(test)]
mod tests;
