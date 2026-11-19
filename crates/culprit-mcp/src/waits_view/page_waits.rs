//! The shared report owns original-frame admission; this transport presents only its recorded context.
use culprit_core::telemetry::{ActivityClient, ActivityKey, DefinitionPayload};
use culprit_core::{PageWaitExplanation, PageWaitRecordedIncident, PageWaitRecordedRequest};
use serde_json::{Value, json};

pub(super) fn recorded_incident(incident: &PageWaitRecordedIncident) -> Value {
    let mut context =
        recorded_context(&incident.observed_at, &incident.request, &incident.sql, incident.client.as_ref());
    context["qualification"] = json!(culprit_core::t!("view.page_waits.recorded.qualification"));
    context["resource"] = json!(incident.resource);
    context
}

pub(super) fn entry(entry: &PageWaitExplanation) -> Value {
    let mut value = json!({
        "observation_key":entry.observation_key,
        "request_key":entry.request_key,
        "resource":entry.resource,
        "blocking":entry.blocking,
        "title":entry.title,
        "explanation":entry.explanation,
        "investigation":entry.investigation,
        "verify":entry.verify,
        "recorded_request":entry.recorded_request.as_ref().map(recorded_request),
    });
    if let Some(task) = &entry.task {
        value["task"] = json!(task);
    }
    value
}

fn recorded_request(request: &PageWaitRecordedRequest) -> Value {
    let mut context = recorded_context(&request.observed_at, &request.request, &request.sql, request.client.as_ref());
    context["qualification"] = json!(culprit_core::t!("view.page_waits.recorded_request.qualification"));
    context
}

fn recorded_context(
    at: &str,
    request: &ActivityKey,
    sql: &DefinitionPayload,
    client: Option<&ActivityClient>,
) -> Value {
    json!({
        "historical_context":true, "observed_at":at, "request":request,
        "statement":crate::statement_context::statement(sql), "client":client,
    })
}

#[cfg(test)]
mod tests;
