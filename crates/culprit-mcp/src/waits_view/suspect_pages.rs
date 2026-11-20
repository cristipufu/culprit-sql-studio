//! Both local-capture and saved-report routes project the same canonical history without private source proof.
use culprit_core::{Report, SuspectPageHistoryReport};
use serde_json::{Value, json};
pub(super) fn attach(response: &mut Value, report: &Report) {
    if let Some(history) = &report.suspect_pages {
        response["suspect_pages"] = value(history)
    }
}
pub(crate) fn value(report: &SuspectPageHistoryReport) -> Value {
    json!({"title":report.title,"summary":report.summary,"source_state":report.source_state,
        "source_reason":report.source_reason,"read_complete":report.read_complete,
        "database":report.database,"native_database_id":report.native_database_id,"observed_at":report.observed_at,
        "active_rows":report.active_rows,"retained_rows":report.retained_rows,
        "last_reported_time_without_offset":report.last_reported_time_without_offset,
        "read_bounds":report.context.as_ref().map(|context|{let(from,until)=context.bounds();json!({"observed_from":from,"observed_until":until})}),
        "metrics":report.metrics,"limitations":report.limitations})
}
#[cfg(test)]
mod tests;
