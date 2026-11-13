//! Current execution reservations identify participants, never the statement that acquired them or a causal owner.
use super::memory_participants;
use culprit_core::{
    Error, MemoryParticipantExplanation, MemoryParticipantsReport, Result, SourceOutcome, format, t,
    telemetry::{
        ExecutionGrantObservation, ExecutionGrantState, GrantStatementRelationship, MAX_REQUEST_MEMORY_OBSERVATIONS,
        RequestGrantAssociation,
    },
};
use std::collections::BTreeSet;

/// Scan and archived evidence use the same bounded native observations. Unknown request ownership remains unknown;
/// reserved, used and peak memory are never interchangeable or summed into an inferred database demand.
pub fn explain(
    observations: &[&ExecutionGrantObservation],
    source: Option<&SourceOutcome>,
) -> Result<MemoryParticipantsReport> {
    let mut cases = validate(observations, source)?;
    cases.sort_by(|left, right| {
        order(left.observation.state).cmp(&order(right.observation.state)).then_with(|| left.key.cmp(&right.key))
    });
    let waiting = observations.iter().filter(|row| row.state == ExecutionGrantState::Waiting).count();
    Ok(MemoryParticipantsReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.request_memory.title"),
        summary: if observations.is_empty() {
            t!("view.request_memory.summary.empty")
        } else {
            t!(
                "view.request_memory.summary.participants",
                n = observations.len(),
                count = format::count(observations.len() as f64),
                waiting = format::count(waiting as f64),
                granted = format::count((observations.len() - waiting) as f64)
            )
        },
        limitations: limitations(observations, source),
        entries: cases.into_iter().map(render).collect(),
    })
}

struct Participant<'a> {
    observation: &'a ExecutionGrantObservation,
    key: String,
}

fn validate<'a>(
    observations: &[&'a ExecutionGrantObservation],
    source: Option<&SourceOutcome>,
) -> Result<Vec<Participant<'a>>> {
    if observations.len() > MAX_REQUEST_MEMORY_OBSERVATIONS
        || !memory_participants::consistent_source(source, observations.iter().map(|row| row.source.as_str()))
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    observations
        .iter()
        .map(|observation| {
            if !observation.is_valid() {
                return Err(invalid());
            }
            let key = observation.key()?;
            if !keys.insert(key.clone()) {
                return Err(invalid());
            }
            Ok(Participant { observation, key })
        })
        .collect()
}

fn limitations(observations: &[&ExecutionGrantObservation], source: Option<&SourceOutcome>) -> Vec<String> {
    let mut limits = vec![
        t!("view.request_memory.limit.observation"),
        t!("view.request_memory.limit.ownership"),
        t!("view.request_memory.limit.clock"),
        t!("view.request_memory.limit.measurements"),
    ];
    if observations.iter().any(|observation| observation.scope.is_none()) {
        limits.push(t!("view.request_memory.limit.unowned"));
    }
    limits.extend(memory_participants::source_limitation(source));
    limits
}

fn order(state: ExecutionGrantState) -> u8 {
    match state {
        ExecutionGrantState::Waiting => 0,
        ExecutionGrantState::Granted => 1,
    }
}

fn render(participant: Participant<'_>) -> MemoryParticipantExplanation {
    let observation = participant.observation;
    let (title, explanation, investigation) = match observation.state {
        ExecutionGrantState::Waiting => (
            observation.requested_kibibytes.as_ref().map_or_else(
                || t!("view.request_memory.waiting.title"),
                |value| t!("view.request_memory.waiting.title.amount", requested = format::digits(value.clone())),
            ),
            t!("view.request_memory.waiting.explanation"),
            t!("view.request_memory.waiting.investigation"),
        ),
        ExecutionGrantState::Granted => (
            observation.granted_kibibytes.as_ref().map_or_else(
                || t!("view.request_memory.granted.title"),
                |value| t!("view.request_memory.granted.title.amount", granted = format::digits(value.clone())),
            ),
            t!("view.request_memory.granted.explanation"),
            t!("view.request_memory.granted.investigation"),
        ),
    };
    let mut details = vec![explanation, request_context(observation)];
    for (value, key) in [
        (&observation.requested_kibibytes, culprit_core::key!("view.request_memory.metric.requested")),
        (&observation.granted_kibibytes, culprit_core::key!("view.request_memory.metric.granted")),
        (&observation.used_kibibytes, culprit_core::key!("view.request_memory.metric.used")),
        (&observation.wait_ms, culprit_core::key!("view.request_memory.metric.wait")),
    ] {
        if let Some(value) = value {
            details.push(culprit_core::i18n::t(key, &[("n", format::digits(value.clone()).into())]));
        }
    }
    MemoryParticipantExplanation {
        observation_key: participant.key,
        observation_identity: observation.identity.clone(),
        title,
        explanation: details.join(" "),
        investigation,
        verify: t!("view.request_memory.verify"),
    }
}

fn request_context(observation: &ExecutionGrantObservation) -> String {
    match observation.request_association {
        RequestGrantAssociation::GenerationChecked => {
            if observation.statement_relationship == GrantStatementRelationship::ObservedCurrentStatement {
                t!("view.request_memory.request.observed_statement")
            } else {
                t!("view.request_memory.request.statement_unavailable")
            }
        }
        RequestGrantAssociation::MissingRequest => t!("view.request_memory.request.missing"),
        RequestGrantAssociation::ChangedRequest => t!("view.request_memory.request.changed"),
        RequestGrantAssociation::InsufficientEvidence => t!("view.request_memory.request.unqualified"),
    }
}

fn invalid() -> Error {
    Error::invalid(t!("view.request_memory.invalid_capture"))
}

#[cfg(test)]
pub(crate) mod tests;
