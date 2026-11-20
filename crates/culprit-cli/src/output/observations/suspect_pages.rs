use super::{SourceQualification, Summary, append_remaining_limitations, render_summary, statement::preview};
use culprit_core::{SuspectPageHistoryReport, t};
pub(crate) fn suspect_pages(report: &SuspectPageHistoryReport) -> String {
    let mut text = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |text| {
            if let (Some(database), Some(id)) = (&report.database, &report.native_database_id) {
                text.push('\n');
                text.push_str(&t!("view.performance_counters.database", database = preview(database), id = id));
            }
            if let Some(context) = &report.context {
                let (from, until) = context.bounds();
                text.push('\n');
                text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
            }
            if let Some(at) = &report.observed_at {
                text.push('\n');
                text.push_str(&t!(
                    "view.counter_ratios.cli.metric",
                    label = t!("view.database_properties.native_clock"),
                    value = at
                ));
            }
            for metric in &report.metrics {
                text.push('\n');
                text.push_str(&t!(
                    "view.counter_ratios.cli.metric",
                    label = &metric.label,
                    value = metric.text.as_deref().unwrap_or_default()
                ));
            }
            if let Some(at) = &report.last_reported_time_without_offset {
                text.push('\n');
                text.push_str(&t!(
                    "view.counter_ratios.cli.metric",
                    label = t!("view.suspect_pages.last_active_time"),
                    value = at
                ));
            }
        },
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}

/// The compact scan response retains the canonical history while keeping authenticated original proof private.
pub(crate) fn compact(report: &SuspectPageHistoryReport) -> serde_json::Value {
    serde_json::json!({
        "title":report.title,"summary":report.summary,"source_state":report.source_state,
        "source_reason":report.source_reason,"read_complete":report.read_complete,
        "database":report.database,"native_database_id":report.native_database_id,"observed_at":report.observed_at,
        "active_rows":report.active_rows,"retained_rows":report.retained_rows,
        "last_reported_time_without_offset":report.last_reported_time_without_offset,
        "read_bounds":report.context.as_ref().map(|context|{
            let(from,until)=context.bounds();
            serde_json::json!({"observed_from":from,"observed_until":until})
        }),
        "metrics":report.metrics,"limitations":report.limitations,
    })
}

#[cfg(test)]
mod tests;
