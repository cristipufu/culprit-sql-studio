use super::{SourceQualification, Summary, render_summary};
use culprit_core::{InstanceResourceReport, format, t};

pub(crate) fn instance_resources(report: &InstanceResourceReport) -> String {
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: Some(report.source_state), reason: None },
            limitations: &report.limitations,
        },
        |text| {
            if let Some(at) = &report.observed_at {
                text.push('\n');
                text.push_str(&t!("view.instance_resources.observed_at", at = at));
            }
            if let Some(context) = &report.context {
                let (from, until) = context.bounds();
                text.push('\n');
                text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
            }
            for group in &report.groups {
                text.push('\n');
                text.push_str(&group.title);
                for field in &group.fields {
                    let value = field
                        .value
                        .as_ref()
                        .map_or_else(|| t!("view.database_properties.unknown"), |value| format::digits(value.clone()));
                    text.push('\n');
                    text.push_str(&t!("view.counter_ratios.cli.metric", label = &field.label, value = value));
                }
            }
        },
    )
}

#[cfg(test)]
mod tests;
