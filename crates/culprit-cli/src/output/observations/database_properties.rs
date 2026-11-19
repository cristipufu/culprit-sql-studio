use super::{SourceQualification, Summary, append_remaining_limitations, render_summary};
use culprit_core::{DatabasePropertiesReport, t};

pub(crate) fn database_properties(report: &DatabasePropertiesReport) -> String {
    let mut text = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: None },
            limitations: &report.limitations,
        },
        |text| {
            if let Some(context) = &report.context {
                let (from, until) = context.bounds();
                text.push('\n');
                text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
            }
            for (label, value) in [
                (t!("view.database_properties.native_clock"), &report.observed_at),
                (t!("view.database_properties.native_database_id"), &report.native_database_id),
            ] {
                if let Some(value) = value {
                    text.push('\n');
                    text.push_str(&t!("view.counter_ratios.cli.metric", label = label, value = value));
                }
            }
            for field in &report.fields {
                text.push('\n');
                let value = field.value.clone().unwrap_or_else(|| t!("view.database_properties.unknown"));
                text.push_str(&t!("view.counter_ratios.cli.metric", label = &field.label, value = value));
                for (label, value) in [
                    (t!("view.database_properties.native_code"), &field.native_code),
                    (t!("view.database_properties.native_description"), &field.native_description),
                ] {
                    if let Some(value) = value {
                        text.push('\n');
                        text.push_str(&t!("view.counter_ratios.cli.metric", label = label, value = value));
                    }
                }
            }
        },
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}

#[cfg(test)]
mod tests;
