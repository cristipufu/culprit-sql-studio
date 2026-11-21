//! Source-free runtime details never reconstruct last values, pooled deviation, allocation owners or causes.
use super::native_statistics;
use culprit_core::{Metric, Result, Snapshot, SourceState, format, key, query_runtime_report::*, t, telemetry::*};
pub const MAX_QUERY_RUNTIME_DETAIL_ENTRIES: usize = 256;
pub fn explain_recorded(original: &RecordedQueryHistoryRead) -> Result<QueryRuntimeFragmentReport> {
    match original {
        RecordedQueryHistoryRead::Captured { read } => explain(read),
        RecordedQueryHistoryRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}
pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<QueryRuntimeFragmentReport>> {
    snapshot.operations.runtime_fragments.as_deref().map(explain_recorded).transpose()
}
pub fn explain(read: &CapturedQueryHistoryRead) -> Result<QueryRuntimeFragmentReport> {
    if !read.is_valid() || read.stream() != Some(TelemetryStream::QueryRuntime) {
        return Err(CollectionStop::InvalidPage.error());
    }
    let observed = read.rows.len();
    let limited = observed > MAX_QUERY_RUNTIME_DETAIL_ENTRIES;
    let mut limitations = vec![
        t!("view.runtime_fragments.limit.fragments"),
        t!("view.runtime_fragments.limit.scope"),
        t!("view.runtime_fragments.limit.memory"),
        t!("view.runtime_fragments.limit.timestamps"),
    ];
    if !read.reference_coverage.complete {
        limitations.push(t!("view.runtime_fragments.limit.references"));
    }
    if !read.read_complete {
        limitations.push(t!("view.runtime_fragments.limit.partial"));
    }
    if limited {
        limitations.push(t!(
            "view.runtime_fragments.limit.presentation",
            count = format::digits(MAX_QUERY_RUNTIME_DETAIL_ENTRIES.to_string())
        ));
    }
    Ok(QueryRuntimeFragmentReport {
        source: read.description.source.clone(),
        source_state: if !read.read_complete || !read.reference_coverage.complete {
            SourceState::Partial
        } else if observed == 0 {
            SourceState::Empty
        } else {
            SourceState::Collected
        },
        source_reason: None,
        title: t!("view.runtime_fragments.title"),
        summary: t!(
            "view.runtime_fragments.summary",
            observed = format::digits(observed.to_string()),
            selected = format::digits(
                read.read
                    .captured_rows(TelemetryStream::QueryRuntime)
                    .ok_or_else(|| CollectionStop::InvalidPage.error())?
                    .to_string()
            )
        ),
        limitations,
        selected_rows: read.read.captured_rows(TelemetryStream::QueryRuntime),
        observed_rows: observed as u32,
        read_complete: read.read_complete,
        references_complete: Some(read.reference_coverage.complete),
        presentation_limited: limited,
        read: Some(Box::new(read.read.clone())),
        description: Some(Box::new(read.description.clone())),
        collected_from: Some(read.collected_from.clone()),
        collected_until: Some(read.collected_until.clone()),
        entries: read.rows.iter().take(MAX_QUERY_RUNTIME_DETAIL_ENTRIES).map(entry).collect::<Result<Vec<_>>>()?,
    })
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> QueryRuntimeFragmentReport {
    QueryRuntimeFragmentReport {
        source: culprit_core::source_id::QUERY_STORE.into(),
        source_state: SourceState::Unavailable,
        source_reason: Some(reason.render()),
        title: t!("view.runtime_fragments.title"),
        summary: t!("view.runtime_fragments.unavailable"),
        limitations: vec![reason.render()],
        selected_rows: None,
        observed_rows: 0,
        read_complete: false,
        references_complete: None,
        presentation_limited: false,
        read: None,
        description: None,
        collected_from: None,
        collected_until: None,
        entries: vec![],
    }
}
fn entry(row: &CapturedQueryHistoryRow) -> Result<QueryRuntimeFragmentEntry> {
    let runtime = row.fragment.runtime().ok_or_else(|| CollectionStop::InvalidPage.error())?;
    Ok(QueryRuntimeFragmentEntry {
        reference: row.reference.clone(),
        native: runtime.key.clone(),
        executions: runtime.executions.clone(),
        first_execution_at: runtime.first_execution_at.clone(),
        last_execution_at: runtime.last_execution_at.clone(),
        metrics: runtime.metrics.iter().map(metric).collect(),
        page_server_reads: runtime.page_server_reads.clone(),
        page_server_explanation: runtime
            .page_server_reads
            .as_deref()
            .map(|statistics| super::page_server_reads::explain_statistics(statistics, None).map(Box::new))
            .transpose()?,
    })
}
fn metric(metric: &RuntimeMetric) -> QueryRuntimeFragmentMetric {
    let value = |value: &MeasurementValue| {
        native_statistics::value(value, metric.unit, key!("view.runtime_fragments.quantity"))
    };
    let number = |value| native_statistics::number(value, metric.unit, key!("view.runtime_fragments.quantity"));
    QueryRuntimeFragmentMetric {
        key: metric.key.clone(),
        unit: metric.unit,
        title: title(&metric.key),
        statistics: [
            (key!("view.runtime_fragments.stat.average"), metric.average.map(number)),
            (key!("view.runtime_fragments.stat.last"), metric.last.as_ref().map(value)),
            (key!("view.runtime_fragments.stat.minimum"), metric.minimum.as_ref().map(value)),
            (key!("view.runtime_fragments.stat.maximum"), metric.maximum.as_ref().map(value)),
            (key!("view.runtime_fragments.stat.deviation"), metric.standard_deviation.map(number)),
        ]
        .into_iter()
        .map(|(key, value)| Metric::said_text(key, value.unwrap_or_else(|| t!("view.runtime_fragments.unknown"))))
        .collect(),
    }
}
fn title(key: &RuntimeMetricKey) -> String {
    match key {
        RuntimeMetricKey::Duration => t!("view.runtime_fragments.metric.duration"),
        RuntimeMetricKey::CpuTime => t!("view.runtime_fragments.metric.cpu"),
        RuntimeMetricKey::LogicalReads => t!("view.runtime_fragments.metric.logical_reads"),
        RuntimeMetricKey::PhysicalReads => t!("view.runtime_fragments.metric.physical_reads"),
        RuntimeMetricKey::PhysicalReadOperations => t!("view.runtime_fragments.metric.read_operations"),
        RuntimeMetricKey::LogicalWrites => t!("view.runtime_fragments.metric.logical_writes"),
        RuntimeMetricKey::Rows => t!("view.runtime_fragments.metric.rows"),
        RuntimeMetricKey::Parallelism => t!("view.runtime_fragments.metric.parallelism"),
        RuntimeMetricKey::PeakMemory => t!("view.runtime_fragments.metric.memory"),
        RuntimeMetricKey::LogBytes => t!("view.runtime_fragments.metric.log_bytes"),
        RuntimeMetricKey::TemporaryStorage => t!("view.runtime_fragments.metric.tempdb"),
        RuntimeMetricKey::Native(name) if name == "mssql.clr-time" => t!("view.runtime_fragments.metric.clr"),
        _ => t!("view.runtime_fragments.metric.other"),
    }
}
#[cfg(test)]
mod tests;
