use culprit_core::{CollectedSourceOverheadAnalysis, SourceOverheadReport, format, t};

const TEMPLATE_LIMIT: usize = 5;

pub(crate) fn source_overhead(report: &SourceOverheadReport) -> String {
    let evidence = &report.evidence;
    let mut text = format!("{}\n{}", report.title, report.summary);
    text.push('\n');
    text.push_str(&match &evidence.finished_at {
        Some(until) => t!("view.source_overhead.cli.bounds", from = &evidence.started_at, until = until),
        None => t!("view.source_overhead.cli.unfinished", from = &evidence.started_at),
    });
    if let Some(count) = evidence.source_queries {
        text.push('\n');
        text.push_str(&t!("view.source_overhead.cli.total", count = format::digits(count.to_string())));
    }
    append_work(
        &mut text,
        evidence.read_metrics.as_ref(),
        &report.template_order,
        report.connection_work.as_ref(),
        report.native_interval.as_ref(),
    );
    for limitation in &report.limitations {
        text.push('\n');
        text.push_str(limitation);
    }
    text
}

pub(crate) fn collected_source_overhead(analysis: &CollectedSourceOverheadAnalysis) -> String {
    let mut text = source_overhead(&analysis.report);
    text.push('\n');
    text.push_str(&t!(
        "view.source_overhead.cli.archive",
        queries = format::digits(analysis.metrics.queries.to_string()),
        bytes = format::digits(analysis.metrics.response_bytes.to_string()),
        proof = format::digits(analysis.metrics.proof_bytes.to_string()),
        objects = format::digits(analysis.metrics.recovery_objects.to_string()),
        recovery_bytes = format::digits(analysis.metrics.recovery_bytes.to_string())
    ));
    text
}

/// Saved history uses the existing bounded template formatter; unavailable originals never get fake clocks.
pub(crate) fn historical_source_overhead(reports: &[culprit_core::HistoricalSourceOverhead]) -> String {
    let mut text = t!("view.source_overhead.saved_summary");
    for report in reports.iter().take(super::super::BRIEF_FINDING_LIMIT) {
        text.push_str("\n\n");
        match report {
            culprit_core::HistoricalSourceOverhead::Available { report, .. } => text.push_str(&source_overhead(report)),
            culprit_core::HistoricalSourceOverhead::Unavailable { reason } => text.push_str(reason),
        }
    }
    if reports.len() > super::super::BRIEF_FINDING_LIMIT {
        text.push('\n');
        text.push_str(&t!(
            "view.source_overhead.history_overflow",
            shown = format::digits(super::super::BRIEF_FINDING_LIMIT.to_string()),
            recorded = format::digits(reports.len().to_string())
        ));
    }
    text
}

#[cfg(test)]
mod tests;

/// Shared numerical/connection/native leaf formatting only; no capture or authority is constructed.
pub(super) fn append_work(
    text: &mut String,
    metrics: Option<&culprit_core::telemetry::ReadWorkSummary>,
    order: &[u32],
    connection: Option<&culprit_core::SourceConnectionExplanation>,
    native: Option<&culprit_core::SourceNativeIntervalExplanation>,
) {
    if let Some(metrics) = metrics {
        for index in order.iter().take(TEMPLATE_LIMIT) {
            let Some(metric) = metrics.templates.get(*index as usize) else { continue };
            text.push_str("\n\n");
            text.push_str(&t!(
                "view.source_overhead.cli.template",
                id = &metric.template_id,
                elapsed = format::duration(metric.client_elapsed_ms),
                completed = format::digits(metric.completed_tasks.to_string()),
                failures = format::digits(metric.failures.to_string())
            ));
            text.push('\n');
            text.push_str(&t!(
                "view.source_overhead.cli.values",
                rows = format::digits(metric.rows_decoded.clone()),
                bytes = format::digits(metric.charged_value_bytes.clone()),
                timeouts = format::digits(metric.timeouts.to_string()),
                cancellations = format::digits(metric.cancellations.to_string()),
                unconfirmed = format::digits(metric.unconfirmed_cancellations.to_string())
            ));
        }
        if order.len() > TEMPLATE_LIMIT {
            text.push('\n');
            text.push_str(&t!(
                "view.source_overhead.cli.presentation",
                shown = format::digits(TEMPLATE_LIMIT.to_string()),
                recorded = format::digits(metrics.templates.len().to_string())
            ));
        }
    }
    if let Some(connection) = connection {
        text.push_str("\n\n");
        text.push_str(&connection.summary);
        for line in connection.observations.iter().chain(&connection.limitations) {
            text.push('\n');
            text.push_str(line);
        }
    }
    if let Some(native) = native {
        text.push_str("\n\n");
        text.push_str(&native.summary);
        for detail in native.measurements.iter().chain(&native.limitations) {
            text.push('\n');
            text.push_str(detail);
        }
    }
}
