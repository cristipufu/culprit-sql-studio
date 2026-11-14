//! Public coverage is an explicit projection, not a partially serialized private native source description.
use culprit_core::{ObservedWaitResource, Result, telemetry::*};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
enum PublicObservation<'a> {
    QueryHistory(&'a QueryHistoryDescription),
    RetainedSamples(&'a SampleHistoryDescription),
    RetainedEvents(&'a EventHistoryDescription),
    Activity(&'a ActivitySnapshotDescription),
    Snapshot(&'a StateSnapshotDescription),
    FocusedPageWaitContext { snapshot: &'a StateSnapshotDescription, result: Box<FocusResult<'a>> },
    DiagnosticContext(&'a DiagnosticCaptureDescription),
}
#[derive(Serialize)]
struct FocusResult<'a> {
    focus: Focus<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lineage: Option<FocusLineage<'a>>,
    observed_from: &'a str,
    observed_until: &'a str,
    source: FocusSourceState,
    request: PageWaitRequestState,
    wait: PageWaitRelation,
}
#[derive(Serialize)]
struct FocusLineage<'a> {
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
impl<'a> FocusLineage<'a> {
    fn new(value: &'a DiagnosticInvestigationLineage) -> Result<Self> {
        Ok(Self {
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
    }
}
#[derive(Serialize)]
struct Focus<'a> {
    key: String,
    parent: &'a DiagnosticEvidenceRef,
    source: &'a str,
    epoch: &'a str,
    original_observed_at: &'a str,
    statement_identity: &'a NativeIdentity,
    resource: &'a ObservedWaitResource,
}

pub(super) fn observation(value: &SourceObservation) -> Result<Value> {
    let projected = match value {
        SourceObservation::QueryHistory(value) => PublicObservation::QueryHistory(value),
        SourceObservation::RetainedSamples(value) => PublicObservation::RetainedSamples(value),
        SourceObservation::RetainedEvents(value) => PublicObservation::RetainedEvents(value),
        SourceObservation::Activity(value) => PublicObservation::Activity(value),
        SourceObservation::Snapshot(value) => PublicObservation::Snapshot(value),
        SourceObservation::FocusedPageWait { snapshot, result } => PublicObservation::FocusedPageWaitContext {
            snapshot,
            result: Box::new(FocusResult {
                focus: Focus {
                    key: result.focus.key()?,
                    parent: &result.focus.parent,
                    source: &result.focus.source,
                    epoch: &result.focus.epoch,
                    original_observed_at: &result.focus.original_observed_at,
                    statement_identity: &result.focus.statement_identity,
                    resource: &result.focus.resource,
                },
                lineage: result.lineage.as_ref().map(FocusLineage::new).transpose()?,
                observed_from: &result.observed_from,
                observed_until: &result.observed_until,
                source: result.source,
                request: result.request,
                wait: result.wait,
            }),
        },
        SourceObservation::Diagnostics(value) => PublicObservation::DiagnosticContext(value),
        SourceObservation::PagedSnapshot(_) => return Err(DeliveryProblem::Integrity.error()),
    };
    serde_json::to_value(projected).map_err(|_| DeliveryProblem::Integrity.error())
}

#[cfg(test)]
mod tests;
