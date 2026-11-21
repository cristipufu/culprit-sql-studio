//! One canonical fragment view for authenticated local, warehouse and saved-original query-history reads.
use culprit_core::{
    Metric, Result, SourceState, format, key,
    query_wait_report::{QueryWaitFragmentEntry, QueryWaitFragmentReport},
    t,
    telemetry::*,
};

pub const MAX_QUERY_WAIT_DETAIL_ENTRIES: usize = 256;

pub fn explain_recorded(original: &RecordedQueryHistoryRead) -> Result<QueryWaitFragmentReport> {
    match original {
        RecordedQueryHistoryRead::Captured { read } => explain(read),
        RecordedQueryHistoryRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}

pub fn explain_snapshot(snapshot: &culprit_core::Snapshot) -> Result<Option<QueryWaitFragmentReport>> {
    snapshot.operations.query_wait_fragments.as_deref().map(explain_recorded).transpose()
}

pub fn explain(read: &CapturedQueryHistoryRead) -> Result<QueryWaitFragmentReport> {
    if !read.is_valid() || read.stream() != Some(TelemetryStream::QueryWaits) {
        return Err(CollectionStop::InvalidPage.error());
    }
    let observed = read.rows.len();
    let selected =
        read.read.captured_rows(TelemetryStream::QueryWaits).ok_or_else(|| CollectionStop::InvalidPage.error())?;
    let presentation_limited = observed > MAX_QUERY_WAIT_DETAIL_ENTRIES;
    let mut limitations = vec![
        t!("view.query_wait_fragments.limit.fragments"),
        t!("view.query_wait_fragments.limit.history"),
        t!("view.query_wait_fragments.limit.scope"),
    ];
    if !read.reference_coverage.complete {
        limitations.push(t!("view.query_wait_fragments.limit.identity"));
    }
    let semantics = read.description.query_history().and_then(|history| history.waits.as_ref());
    if semantics.is_none_or(|semantics| {
        semantics.capture != WaitCaptureState::Enabled || semantics.coverage != WaitCaptureCoverage::Qualified
    }) {
        limitations.push(t!("view.query_wait_fragments.limit.capture"));
    }
    if !read.read_complete {
        limitations.push(t!("view.query_wait_fragments.limit.partial"));
    }
    if presentation_limited {
        limitations.push(t!(
            "view.query_wait_fragments.limit.presentation",
            count = format::digits(MAX_QUERY_WAIT_DETAIL_ENTRIES.to_string())
        ));
    }
    Ok(QueryWaitFragmentReport {
        source: read.description.source.clone(),
        source_state: if !read.read_complete || !read.reference_coverage.complete {
            SourceState::Partial
        } else if observed == 0 {
            SourceState::Empty
        } else {
            SourceState::Collected
        },
        source_reason: None,
        title: t!("view.query_wait_fragments.title"),
        summary: t!(
            "view.query_wait_fragments.summary",
            observed = format::digits(observed.to_string()),
            selected = format::digits(selected.to_string())
        ),
        limitations,
        selected_rows: Some(selected),
        observed_rows: observed as u32,
        read_complete: read.read_complete,
        capture: semantics.map(|semantics| semantics.capture),
        interval_coverage: semantics.map(|semantics| semantics.coverage),
        references_complete: Some(read.reference_coverage.complete),
        presentation_limited,
        read: Some(Box::new(read.read.clone())),
        description: Some(Box::new(read.description.clone())),
        collected_from: Some(read.collected_from.clone()),
        collected_until: Some(read.collected_until.clone()),
        entries: read.rows.iter().take(MAX_QUERY_WAIT_DETAIL_ENTRIES).map(entry).collect::<Result<Vec<_>>>()?,
    })
}

pub fn unavailable(reason: &culprit_core::i18n::Message) -> QueryWaitFragmentReport {
    QueryWaitFragmentReport {
        source: culprit_core::source_id::QUERY_STORE.into(),
        source_state: SourceState::Unavailable,
        source_reason: Some(reason.render()),
        title: t!("view.query_wait_fragments.title"),
        summary: t!("view.query_wait_fragments.unavailable"),
        limitations: vec![reason.render()],
        selected_rows: None,
        observed_rows: 0,
        read_complete: false,
        capture: None,
        interval_coverage: None,
        references_complete: None,
        presentation_limited: false,
        read: None,
        description: None,
        collected_from: None,
        collected_until: None,
        entries: vec![],
    }
}

fn entry(row: &CapturedQueryHistoryRow) -> Result<QueryWaitFragmentEntry> {
    let wait = row.fragment.wait().ok_or_else(|| CollectionStop::InvalidPage.error())?;
    Ok(QueryWaitFragmentEntry {
        reference: row.reference.clone(),
        native: wait.key.clone(),
        category: wait.category.clone(),
        category_description: wait.category_description.clone(),
        outcome_description: wait.outcome_description.clone(),
        unit: wait.unit,
        metrics: [
            (key!("view.query_wait_fragments.metric.total"), Some(value(&wait.total, wait.unit))),
            (key!("view.query_wait_fragments.metric.average"), wait.average.map(|v| number(v, wait.unit))),
            (key!("view.query_wait_fragments.metric.minimum"), wait.minimum.as_ref().map(|v| value(v, wait.unit))),
            (key!("view.query_wait_fragments.metric.maximum"), wait.maximum.as_ref().map(|v| value(v, wait.unit))),
            (key!("view.query_wait_fragments.metric.last"), wait.last.as_ref().map(|v| value(v, wait.unit))),
            (key!("view.query_wait_fragments.metric.deviation"), wait.standard_deviation.map(|v| number(v, wait.unit))),
        ]
        .into_iter()
        .map(|(key, value)| Metric::said_text(key, value.unwrap_or_else(|| t!("view.query_wait_fragments.unknown"))))
        .collect(),
    })
}

fn value(value: &MeasurementValue, unit: RuntimeMetricUnit) -> String {
    super::native_statistics::value(value, unit, key!("view.query_wait_fragments.quantity"))
}
fn number(value: f64, unit: RuntimeMetricUnit) -> String {
    super::native_statistics::number(value, unit, key!("view.query_wait_fragments.quantity"))
}

#[cfg(test)]
mod tests;
