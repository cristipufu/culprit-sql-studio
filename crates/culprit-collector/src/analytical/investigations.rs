//! Public control provenance retains recovery pointers without duplicating private questions or source authority.
use super::{projection::ScalarType, schema::Field};
use culprit_core::{Result, telemetry::*};
use serde::Serialize;
use serde_json::{Value, json};

pub(super) const CONTEXT_FIELD: Field =
    Field::optional("InvestigationContext", "/_culprit/investigation", ScalarType::JsonContainer);

#[derive(Serialize)]
struct DiagnosticContext<'a> {
    controller_version: u32,
    parent: &'a DiagnosticCaptureRef,
    step_id: &'a str,
    analyzed_at: &'a str,
    records: u32,
    checkpoint_records: u32,
    scope_keys: Vec<&'a str>,
}

pub(super) fn diagnostic_context(value: &DiagnosticCaptureDescription) -> Result<Value> {
    serde_json::to_value(DiagnosticContext {
        controller_version: value.controller_version,
        parent: &value.parent,
        step_id: &value.step_id,
        analyzed_at: &value.analyzed_at,
        records: value.records,
        checkpoint_records: value.checkpoint_records,
        scope_keys: value.scope_bindings.keys().map(String::as_str).collect(),
    })
    .map_err(|_| DeliveryProblem::Integrity.error())
}

#[derive(Serialize)]
struct PublicLineage<'a> {
    intent_key: &'a str,
    episode_id: &'a str,
    episode_revision: &'a str,
    recipe_id: &'a str,
    recipe_version: u32,
    budget: &'a CollectionBudget,
    component: &'a culprit_core::ComponentVersion,
    qualification_key: String,
    cost_evidence: &'a DiagnosticEvidenceRef,
}

pub(super) fn lineage(value: &DiagnosticInvestigationLineage) -> Result<Value> {
    serde_json::to_value(PublicLineage {
        intent_key: &value.intent_key,
        episode_id: &value.episode_id,
        episode_revision: &value.episode_revision,
        recipe_id: &value.recipe_id,
        recipe_version: value.recipe_version,
        budget: &value.budget,
        component: &value.qualification.component,
        qualification_key: value.qualification.key()?,
        cost_evidence: &value.qualification.evidence,
    })
    .map_err(|_| DeliveryProblem::Integrity.error())
}

pub(super) fn event(value: &DiagnosticInvestigationEvent) -> Result<Value> {
    let provenance = DiagnosticInvestigationLineage::from_intent(&value.intent)?;
    Ok(json!({
        "key": value.key()?,
        "lineage": lineage(&provenance)?,
        "attempt": value.attempt,
        "source_work": value.source_work,
        "outcome": value.outcome,
    }))
}
