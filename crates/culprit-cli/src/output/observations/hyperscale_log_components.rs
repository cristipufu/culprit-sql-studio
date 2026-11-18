use super::{SourceQualification, Summary, render_summary};
use culprit_core::{HyperscaleLogComponentReport, t};
pub(crate) fn hyperscale_log_components(report: &HyperscaleLogComponentReport) -> String {
    let mut text = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source_state),
                reason: report.source_reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        |text| {
            text.push('\n');
            text.push_str(&t!(
                "view.connection_outcomes.cli.bounds",
                from = &report.context.observed_from,
                until = &report.context.observed_until
            ));
            for entry in report.entries.iter().take(5) {
                text.push_str("\n\n");
                text.push_str(&entry.title);
                text.push('\n');
                text.push_str(&entry.explanation);
                for (label, value) in [
                    (t!("view.hyperscale_log_components.field.component"), Some(entry.component_id.as_str())),
                    (t!("view.hyperscale_log_components.field.replica"), entry.replica_id.as_deref()),
                    (t!("view.hyperscale_log_components.field.file"), entry.file_id.as_deref()),
                    (
                        t!("view.hyperscale_log_components.field.clock"),
                        entry.last_reported_time_without_offset.as_deref(),
                    ),
                ] {
                    text.push('\n');
                    text.push_str(&t!(
                        "view.counter_ratios.cli.metric",
                        label = label,
                        value =
                            value.map(str::to_owned).unwrap_or_else(|| t!("view.hyperscale_log_components.unknown"))
                    ));
                }
                for metric in &entry.metrics {
                    text.push('\n');
                    text.push_str(&t!(
                        "view.counter_ratios.cli.metric",
                        label = &metric.label,
                        value = metric.text.as_deref().unwrap_or("")
                    ));
                }
            }
        },
    );
    for limitation in report.limitations.iter().skip(1) {
        text.push('\n');
        text.push_str(limitation);
    }
    if report.observed_rows > 5 {
        text.push('\n');
        text.push_str(&t!(
            "view.hyperscale_log_components.presented",
            shown = culprit_core::format::digits(report.entries.len().min(5).to_string()),
            total = culprit_core::format::digits(report.observed_rows.to_string())
        ));
    }
    text
}
