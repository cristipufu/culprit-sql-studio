use super::{OBSERVATION_DETAIL_CHARS, SourceQualification, Summary, native_work_summary, render_summary, statement};
use culprit_core::{InstanceResourceReport, SchedulerFieldAvailability, SchedulerReport, format, t};

const MAX_SCHEDULER_FIELDS: usize = culprit_core::telemetry::MSSQL_SCHEDULER_FIELDS.len() - 1;
const MAX_SCHEDULER_LIMITATIONS: usize = 8;

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
            if let Some(schedulers) = &report.schedulers {
                text.push('\n');
                text.push_str(&scheduler_text(schedulers));
            }
            if let Some(cpu) = &report.process_cpu {
                text.push('\n');
                text.push_str(&render_summary(
                    Summary {
                        title: &cpu.title,
                        summary: &cpu.summary,
                        source: SourceQualification::Single { state: Some(cpu.source_state), reason: None },
                        limitations: &cpu.limitations,
                    },
                    |text| {
                        if let Some(at) = &cpu.observed_at {
                            text.push('\n');
                            text.push_str(&t!("view.instance_resources.observed_at", at = at));
                        }
                        for field in &cpu.fields {
                            let value = field.value.as_ref().map_or_else(
                                || t!("view.database_properties.unknown"),
                                |value| format::digits(value.clone()),
                            );
                            text.push('\n');
                            text.push_str(&t!("view.counter_ratios.cli.metric", label = &field.label, value = value));
                        }
                    },
                ));
            }
        },
    )
}

#[cfg(test)]
mod tests;

fn scheduler_text(report: &SchedulerReport) -> String {
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source_state),
                reason: report.source_reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let fields = entry
                .fields
                .iter()
                .take(MAX_SCHEDULER_FIELDS)
                .map(|field| {
                    let value = match field.availability {
                        SchedulerFieldAvailability::Observed => field
                            .value
                            .as_deref()
                            .map(statement::preview)
                            .unwrap_or_else(|| t!("view.database_properties.unknown")),
                        SchedulerFieldAvailability::NativeNull => t!("view.schedulers.value.null"),
                        SchedulerFieldAvailability::ColumnUnavailable => t!("view.schedulers.value.unavailable"),
                        SchedulerFieldAvailability::InstrumentationUnqualified => field
                            .unqualified
                            .as_ref()
                            .map(|raw| {
                                t!("view.schedulers.value.unqualified", value = statement::preview(&raw.native_value))
                            })
                            .unwrap_or_else(|| t!("view.schedulers.value.unavailable")),
                    };
                    let mut line =
                        t!("view.counter_ratios.cli.metric", label = statement::preview(&field.label), value = value);
                    if let Some(explanation) = &field.explanation {
                        line.push(' ');
                        line.push_str(&statement::preview_bounded(explanation, OBSERVATION_DETAIL_CHARS));
                    }
                    line
                })
                .collect();
            (entry.title.as_str(), fields)
        }),
    );
    text.push('\n');
    text.push_str(&report.population);
    if let Some(context) = &report.context {
        let (from, until) = context.bounds();
        text.push('\n');
        text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
    }
    text.push('\n');
    text.push_str(&t!(
        "view.schedulers.entries_displayed",
        shown = format::digits(report.entries.len().min(super::super::BRIEF_FINDING_LIMIT).to_string()),
        available = format::digits(report.entries.len().to_string())
    ));
    for limitation in report.limitations.iter().skip(1).take(MAX_SCHEDULER_LIMITATIONS - 1) {
        text.push('\n');
        text.push_str(&statement::preview_bounded(limitation, OBSERVATION_DETAIL_CHARS));
    }
    text
}
