//! Display native phase facts independently; no totals, rates or stage-to-pressure inference.
use culprit_core::{Metric, format, key, t, telemetry::MssqlQueryCompilation};

pub(super) fn metrics(row: &MssqlQueryCompilation) -> Vec<Metric> {
    let native = &row.timings;
    [
        (key!("view.compilation.metric.last_duration"), native.last_duration_us.clone()),
        (
            key!("view.compilation.metric.average_bind_duration"),
            native.binding.average_duration_us.map(|value| value.to_string()),
        ),
        (key!("view.compilation.metric.last_bind_duration"), native.binding.last_duration_us.clone()),
        (
            key!("view.compilation.metric.average_bind_cpu"),
            native.binding.average_cpu_us.map(|value| value.to_string()),
        ),
        (key!("view.compilation.metric.last_bind_cpu"), native.binding.last_cpu_us.clone()),
        (
            key!("view.compilation.metric.average_optimize_duration"),
            native.optimization.average_duration_us.map(|value| value.to_string()),
        ),
        (key!("view.compilation.metric.last_optimize_duration"), native.optimization.last_duration_us.clone()),
        (
            key!("view.compilation.metric.average_optimize_cpu"),
            native.optimization.average_cpu_us.map(|value| value.to_string()),
        ),
        (key!("view.compilation.metric.last_optimize_cpu"), native.optimization.last_cpu_us.clone()),
    ]
    .into_iter()
    .map(|(key, value)| {
        Metric::said_text(
            key,
            value
                .map(|value| t!("common.duration.us", n = format::digits(value)))
                .unwrap_or_else(|| t!("view.compilation.timing_unknown")),
        )
    })
    .collect()
}

#[cfg(test)]
mod tests;
