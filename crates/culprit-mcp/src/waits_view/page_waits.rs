//! The shared report owns original-frame admission; this transport presents only its recorded context.
use culprit_core::PageWaitRecordedIncident;
use serde_json::{Value, json};

pub(super) fn recorded_incident(incident: &PageWaitRecordedIncident) -> Value {
    json!({
        "historical_context":true,
        "qualification":culprit_core::t!("view.page_waits.recorded.qualification"),
        "observed_at":incident.observed_at,
        "request":incident.request,
        "resource":incident.resource,
        "statement":crate::statement_context::statement(&incident.sql),
        "client":incident.client,
    })
}

#[cfg(test)]
mod tests;
