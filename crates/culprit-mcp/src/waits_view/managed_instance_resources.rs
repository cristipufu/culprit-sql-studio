//! Compact canonical historical intervals; no original receipts/current member proof or unrelated pressure.
use culprit_core::{ManagedInstanceResourceReport, Report, SourceState};
use serde_json::{Value, json};
const INTERVAL_LIMIT: usize = 3;
const FIELD_LIMIT: usize = 11;
fn text(value: &str) -> String {
    crate::statement_context::bounded_text(value, 1024)
}
pub(crate) fn value(report: &ManagedInstanceResourceReport) -> Value {
    let admitted = matches!(report.source_state, SourceState::Collected | SourceState::Partial);
    json!({"title":text(&report.title),"summary":text(&report.summary),"scope":"shared-instance-history","source_state":report.source_state,"read_complete":report.read_complete,
        "from":report.from.as_deref().map(text),"until":report.until.as_deref().map(text),"observed_at":report.observed_at.as_deref().map(text),"native_source":report.native_source.as_ref().map(|source|json!({"source":text(&source.source),"state":source.state,"records":source.records,"complete_scope":source.complete_scope,"error_code":source.error_code,"stopped":source.stopped})),
        "layout":report.layout,"native_rows":report.native_rows,"recorded_rows":report.recorded_rows,"observed_rows":report.observed_rows,"intervals_limited":report.intervals.len()>INTERVAL_LIMIT,
        "intervals":report.intervals.iter().filter(|_|admitted).take(INTERVAL_LIMIT).map(|interval|json!({"started_at":text(&interval.started_at),"ended_at":text(&interval.ended_at),"fields":interval.fields.iter().take(FIELD_LIMIT).map(|field|json!({"key":text(&field.key),"label":text(&field.label),"availability":field.availability,"native_value":field.native_value.as_deref().map(text),"value":field.value.as_deref().map(text),"missing_reason":field.missing_reason.as_deref().map(text)})).collect::<Vec<_>>() })).collect::<Vec<_>>(),
        "limitations":report.limitations.iter().take(8).map(|value|text(value)).collect::<Vec<_>>(),"investigation":text(&report.investigation),"verify":text(&report.verify)})
}
pub(super) fn attach(response: &mut Value, report: &Report) {
    if let Some(history) = &report.managed_instance_resources {
        response["managed_instance_resources"] = value(history);
    }
}
#[cfg(test)]
mod tests;
