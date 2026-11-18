//! Configured rates retain separate native limits; no utilization, current limiter or query cause is inferred.
use culprit_core::{
    Comparison, ComparisonReason, ConfiguredLogRateEntry, ConfiguredLogRateReport, ConfiguredLogRateScope, Metric,
    Result, SourceState, format, t, telemetry::ConfiguredLogRateRead,
};

pub fn explain_snapshot(snapshot: &culprit_core::Snapshot) -> Result<Option<ConfiguredLogRateReport>> {
    let Some(read) = snapshot.pressure.as_ref().and_then(|pressure| pressure.configured_log_rates.as_deref()) else {
        return Ok(None);
    };
    match read.context.target.snapshot_namespace_comparison(snapshot) {
        Comparison::Compatible => explain(read, true).map(Some),
        Comparison::Incompatible(ComparisonReason::UnknownIdentity) => explain(read, true).map(|mut report| {
            report.limitations.push(t!("view.observations.limit.namespace"));
            Some(report)
        }),
        _ => Err(invalid()),
    }
}

pub fn explain(read: &ConfiguredLogRateRead, read_complete: bool) -> Result<ConfiguredLogRateReport> {
    if !read.is_valid_read(read_complete) {
        return Err(invalid());
    }
    let point = read.observation.as_deref();
    let mut limitations = vec![
        t!("view.configured_log_rates.limit.configuration"),
        t!("view.configured_log_rates.limit.components"),
        t!("view.configured_log_rates.limit.scopes"),
        t!("view.configured_log_rates.limit.history"),
    ];
    if point.is_some_and(|point| point.workload_group_id.is_none()) || !read.context.has_qualified_local_owner() {
        limitations.push(t!("view.configured_log_rates.limit.identity"));
    }
    if point.is_some_and(|point| point.observed_at.is_none()) {
        limitations.push(t!("view.configured_log_rates.limit.clock"));
    }
    if !read_complete || point.is_none() && read.source.records != 0 || read.source.state == SourceState::Partial {
        limitations.push(t!("view.configured_log_rates.limit.partial"));
    }
    let entries = point
        .map(|point| {
            vec![
                entry(ConfiguredLogRateScope::UserWorkloadGroup, point.workload_group_bytes_per_second.as_deref()),
                entry(ConfiguredLogRateScope::UserResourcePool, point.resource_pool_bytes_per_second.as_deref()),
                entry(ConfiguredLogRateScope::DatabaseEngineInstance, point.instance_bytes_per_second.as_deref()),
            ]
        })
        .unwrap_or_default();
    Ok(ConfiguredLogRateReport {
        context: read.context.clone(),
        source_state: read.source.state,
        source_reason: (read.source.state == SourceState::Unavailable)
            .then(|| t!("view.configured_log_rates.unavailable")),
        read_complete,
        native_rows: read.source.records,
        observed_rows: point.is_some() as u32,
        observed_at: point.and_then(|point| point.observed_at.clone()),
        workload_group_id: point.and_then(|point| point.workload_group_id.clone()),
        title: t!("view.configured_log_rates.title"),
        summary: if point.is_some() {
            t!("view.configured_log_rates.summary.observed")
        } else {
            t!("view.configured_log_rates.summary.unavailable")
        },
        limitations,
        entries,
    })
}

fn entry(scope: ConfiguredLogRateScope, value: Option<&str>) -> ConfiguredLogRateEntry {
    let (title, explanation) = match scope {
        ConfiguredLogRateScope::UserWorkloadGroup => (
            t!("view.configured_log_rates.scope.workload_group"),
            t!("view.configured_log_rates.meaning.workload_group"),
        ),
        ConfiguredLogRateScope::UserResourcePool => {
            (t!("view.configured_log_rates.scope.resource_pool"), t!("view.configured_log_rates.meaning.resource_pool"))
        }
        ConfiguredLogRateScope::DatabaseEngineInstance => {
            (t!("view.configured_log_rates.scope.instance"), t!("view.configured_log_rates.meaning.instance"))
        }
    };
    let display = value
        .map(|value| t!("view.configured_log_rates.rate", value = format::digits(value.to_owned())))
        .unwrap_or_else(|| t!("view.configured_log_rates.unknown"));
    ConfiguredLogRateEntry {
        scope,
        bytes_per_second: value.map(str::to_owned),
        title,
        explanation,
        metric: Metric::text(t!("view.configured_log_rates.metric.limit"), display)
            .keyed("view.configured_log_rates.metric.limit"),
    }
}

fn invalid() -> culprit_core::Error {
    culprit_core::Error::invalid(t!("view.configured_log_rates.invalid"))
}

#[cfg(test)]
mod tests;
