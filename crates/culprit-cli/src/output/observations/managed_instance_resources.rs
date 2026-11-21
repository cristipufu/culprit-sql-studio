//! The canonical interpreter owns native meanings; the terminal only bounds and escapes derived fields.
use super::{SourceQualification, Summary, append_remaining_limitations, render_summary, statement};
use culprit_core::{ManagedInstanceResourceReport, SourceState, t};
const INTERVAL_LIMIT: usize = 3;
const FIELD_LIMIT: usize = 11;
const TEXT_LIMIT: usize = 1024;
fn text(value: &str) -> String {
    statement::preview_bounded(value, TEXT_LIMIT)
}
pub(crate) fn managed_instance_resources(report: &ManagedInstanceResourceReport) -> String {
    let title = text(&report.title);
    let summary = text(&report.summary);
    let limitations = report.limitations.iter().take(8).map(|value| text(value)).collect::<Vec<_>>();
    let mut result = render_summary(
        Summary {
            title: &title,
            summary: &summary,
            source: SourceQualification::Single { state: Some(report.source_state), reason: None },
            limitations: &limitations,
        },
        |output| {
            let mut lines = vec![];
            if let (Some(from), Some(until)) = (&report.from, &report.until) {
                lines.push(t!("view.mi_resources.window", from = text(from), until = text(until)));
            }
            if let Some(at) = &report.observed_at {
                lines.push(t!("view.mi_resources.observed", at = text(at)));
            }
            if matches!(report.source_state, SourceState::Collected | SourceState::Partial) {
                for interval in report.intervals.iter().take(INTERVAL_LIMIT) {
                    lines.push(t!(
                        "view.mi_resources.interval",
                        from = text(&interval.started_at),
                        until = text(&interval.ended_at)
                    ));
                    for field in interval.fields.iter().take(FIELD_LIMIT) {
                        lines.push(t!(
                            "evidence.cli.replication_field",
                            label = text(&field.label),
                            value = field
                                .value
                                .as_deref()
                                .or(field.missing_reason.as_deref())
                                .map(text)
                                .unwrap_or_else(|| t!("ui.memory_failures.not_reported"))
                        ));
                    }
                }
            }
            lines.push(text(&report.investigation));
            lines.push(text(&report.verify));
            // The existing summary renderer owns its leading qualification; native fields each have their own cap.
            for line in lines {
                output.push('\n');
                output.push_str(&line);
            }
        },
    );
    append_remaining_limitations(&mut result, &limitations);
    result
}
pub(crate) fn compact(report: &ManagedInstanceResourceReport) -> serde_json::Value {
    let admitted = matches!(report.source_state, SourceState::Collected | SourceState::Partial);
    serde_json::json!({"title":text(&report.title),"summary":text(&report.summary),"scope":"shared-instance-history","source_state":report.source_state,"read_complete":report.read_complete,
        "from":report.from.as_deref().map(text),"until":report.until.as_deref().map(text),"observed_at":report.observed_at.as_deref().map(text),"native_rows":report.native_rows,"recorded_rows":report.recorded_rows,"observed_rows":report.observed_rows,
        "intervals_limited":report.intervals.len()>INTERVAL_LIMIT,"intervals":report.intervals.iter().filter(|_|admitted).take(INTERVAL_LIMIT).map(|interval|serde_json::json!({"started_at":text(&interval.started_at),"ended_at":text(&interval.ended_at),"fields":interval.fields.iter().take(FIELD_LIMIT).map(|field|serde_json::json!({"key":text(&field.key),"label":text(&field.label),"availability":field.availability,"native_value":field.native_value.as_deref().map(text),"value":field.value.as_deref().map(text),"missing_reason":field.missing_reason.as_deref().map(text)})).collect::<Vec<_>>() })).collect::<Vec<_>>(),
        "limitations":report.limitations.iter().take(8).map(|value|text(value)).collect::<Vec<_>>(),"investigation":text(&report.investigation),"verify":text(&report.verify)})
}
#[cfg(test)]
mod tests;
