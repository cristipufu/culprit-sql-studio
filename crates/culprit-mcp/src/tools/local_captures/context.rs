//! Compact database-reported context. Callers supply the existing typed relationship; names never establish owners.
use culprit_core::{
    Result, SessionStatementContext,
    telemetry::{
        ActivityFrameEvidence, ActivityKey, ActivityObservation, CollectionDescription, DefinitionPayload,
        DeliveryProblem, DiagnosticEvidenceRef, FocusSourceState, FocusedRequestBinding, NativeRecordIdentity,
        OperationalFact, SourceObservation,
    },
};
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};

const SQL_PREVIEW_CHARS: usize = 512;

pub(super) struct SqlPreview {
    pub text: String,
    pub truncated: bool,
}
impl SqlPreview {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.chars().take(SQL_PREVIEW_CHARS).collect(),
            truncated: text.chars().count() > SQL_PREVIEW_CHARS,
        }
    }
}

pub(super) fn statement(payload: &DefinitionPayload) -> Value {
    match payload {
        DefinitionPayload::Available { text, .. } => {
            let preview = SqlPreview::new(text);
            json!({"availability":"available", "preview":preview.text, "preview_truncated":preview.truncated})
        }
        DefinitionPayload::Unavailable { reason } => json!({"availability":"unavailable", "reason":reason}),
    }
}

/// This is one observed request's context, never an allocator, business service or source-code ownership claim.
/// A withheld statement stays absent even if the surrounding observation retains a native SQL payload.
pub(super) fn participant(
    row: &ActivityObservation,
    observed_at: &str,
    qualified_statement: Option<&DefinitionPayload>,
) -> Value {
    let qualified_statement =
        qualified_statement.filter(|_| row.key.request.is_some() && row.has_session_generation_authority());
    json!({
        "actor":row.key, "observed_at":observed_at, "scope":row.scope,
        "state":row.state, "command":row.command, "client":row.client,"database":row.database,
        "idle":row.idle,"session_generation_disqualified":!row.has_session_generation_authority(),
        "statement":qualified_statement.map(statement),
    })
}

/// The frame already validated every canonical body and receipt. Select the exact original reference, not just
/// a same-key actor in a different capture; this structural selection grants no current ownership.
pub(super) fn referenced_participant(
    frame: &ActivityFrameEvidence<'_>,
    reference: &DiagnosticEvidenceRef,
) -> Result<Value> {
    let NativeRecordIdentity::Activity(key) = &reference.native_key else { return Err(invalid()) };
    let row = frame.row(key).ok_or_else(invalid)?;
    if row.reference != Some(reference) {
        return Err(invalid());
    }
    let mut value = participant(
        row.observation,
        &frame.native().at,
        (row.observation.key.request.is_some() && frame.has_session_generation_authority(&key.session))
            .then_some(&row.observation.statement),
    );
    value["session_generation_disqualified"] = json!(frame.session_generation_is_disqualified(&key.session));
    Ok(value)
}

/// Only the existing graph's qualified exact Activity key can select a current node's native context.
pub(super) fn graph_participant(
    frame: &ActivityFrameEvidence<'_>,
    key: &ActivityKey,
    statement_context: Option<SessionStatementContext>,
) -> Result<Value> {
    let row = frame.row(key).ok_or_else(invalid)?;
    if !frame.context().has_qualified_local_owner()
        || !frame.has_session_generation_authority(&row.observation.key.session)
    {
        return Err(invalid());
    }
    Ok(participant(
        row.observation,
        &frame.native().at,
        (statement_context == Some(SessionStatementContext::CurrentRequest)).then_some(&row.observation.statement),
    ))
}

/// Match the original through the existing native result, not a session-number lookup in a later frame.
pub(super) fn focused_original(source: &CollectionDescription) -> Option<Value> {
    let focus = match &source.observation {
        SourceObservation::CurrentRequestWork { result, .. }
        | SourceObservation::MaintenanceProgress { result, .. }
            if result.binding == FocusedRequestBinding::Matched =>
        {
            result.focus.as_deref()?
        }
        SourceObservation::FocusedRequestMemory { result, .. } if result.source == FocusSourceState::Matched => {
            &result.focus
        }
        _ => return None,
    };
    Some(participant(&focus.original, &focus.original_observed_at, Some(&focus.original.statement)))
}

/// These report families already require one declared source and unique original observation keys.
/// A broken report reference must fail, rather than quietly selecting another participant or hiding its absence.
pub(super) fn single_source_fact<'a>(analysis: &'a LocalSnapshotAnalysis, key: &str) -> Result<&'a OperationalFact> {
    analysis
        .original()
        .read()
        .observations
        .iter()
        .find(|row| row.key == key)
        .map(|row| &row.fact)
        .ok_or_else(|| DeliveryProblem::Integrity.error())
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests;
