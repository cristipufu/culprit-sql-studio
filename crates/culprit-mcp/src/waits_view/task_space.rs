//! Present only canonical task-report context. This transport grants no SQL from the raw task population.
use culprit_core::{TaskSpaceCurrentRequest, TaskSpaceExplanation};
use serde_json::{Value, json};

pub(crate) fn entry(entry: &TaskSpaceExplanation) -> Value {
    json!({
        "observation_key":entry.observation_key, "task":entry.task, "activity":entry.activity,
        "title":entry.title, "explanation":entry.explanation,
        "investigation":entry.investigation, "verify":entry.verify,
        "current_request":entry.current_request.as_ref().map(current_request),
    })
}

pub(crate) fn current_request(request: &TaskSpaceCurrentRequest) -> Value {
    json!({
        "request":request.request,
        "request_check_bounds":{"from":request.observed_from,"until":request.observed_until},
        "statement":crate::statement_context::statement(&request.statement), "client":request.client,
        "qualification":culprit_core::t!("view.task_space.current_request.qualification"),
    })
}

#[cfg(test)]
mod tests;
