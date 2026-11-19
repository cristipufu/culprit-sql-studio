//! Current execution reservations identify participants, never the statement that acquired them or a causal owner.
use super::memory_participants;
use crate::support::request_memory::{Participant, qualified_participants};
use culprit_core::{
    MemoryGrantAdmission, MemoryParticipantExplanation, MemoryParticipantMechanism, MemoryParticipantReference,
    MemoryParticipantsReport, Result, SourceOutcome, format, t,
    telemetry::{
        ExecutionGrantObservation, ExecutionGrantState, GrantStatementRelationship, NativeIdentity, NativeReadContext,
        RequestGrantAssociation,
    },
};
use std::collections::BTreeMap;

pub struct RequestMemoryEvidence<'a> {
    pub context: &'a NativeReadContext,
    pub observations: &'a [&'a ExecutionGrantObservation],
}

/// Scan and archived evidence use the same bounded native observations. Unknown request ownership remains unknown;
/// reserved, used and peak memory are never interchangeable or summed into an inferred database demand.
pub fn explain(
    evidence: RequestMemoryEvidence<'_>,
    source: Option<&SourceOutcome>,
) -> Result<MemoryParticipantsReport> {
    let observations = evidence.observations;
    let mut cases = qualified_participants(evidence.context, evidence.observations, source)?;
    cases.sort_by(|left, right| {
        order(left.observation().state).cmp(&order(right.observation().state)).then_with(|| left.key().cmp(right.key()))
    });
    let waiting = observations.iter().filter(|row| row.state == ExecutionGrantState::Waiting).count();
    let admission_resources = admission_resources(&evidence, &cases);
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
        limitations: limitations(&evidence, source),
        entries: cases.into_iter().map(|participant| render(participant, &admission_resources)).collect(),
        admission_resources,
    })
}

fn admission_resources(
    evidence: &RequestMemoryEvidence<'_>,
    participants: &[Participant<'_>],
) -> Vec<MemoryGrantAdmission> {
    if !evidence.context.has_qualified_local_owner() {
        return vec![];
    }
    let mut resources: BTreeMap<NativeIdentity, Vec<MemoryParticipantReference>> = BTreeMap::new();
    for participant in participants {
        let row = participant.observation();
        if let Some(resource) = participant.resource() {
            resources.entry(resource.clone()).or_default().push(MemoryParticipantReference {
                observation_key: participant.key().to_owned(),
                observation_identity: row.identity.clone(),
            });
        }
    }
    resources.into_iter().map(|(resource, participants)| MemoryGrantAdmission { resource, participants }).collect()
}

fn limitations(evidence: &RequestMemoryEvidence<'_>, source: Option<&SourceOutcome>) -> Vec<String> {
    let observations = evidence.observations;
    let mut limits = vec![
        t!("view.request_memory.limit.observation"),
        t!("view.request_memory.limit.ownership"),
        t!("view.request_memory.limit.clock"),
        t!("view.request_memory.limit.measurements"),
        t!("view.request_memory.limit.population"),
    ];
    if observations.iter().any(|observation| observation.scope.is_none()) {
        limits.push(t!("view.request_memory.limit.unowned"));
    }
    if observations.iter().any(|observation| observation.resource.is_none()) {
        limits.push(t!("view.request_memory.limit.resource"));
    }
    if !evidence.context.has_qualified_local_owner() {
        limits.push(t!("view.request_memory.limit.owner"));
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

struct ParticipantWords {
    title: String,
    explanation: String,
    investigation: String,
    verify: String,
}

fn participant_words(participant: &Participant<'_>) -> ParticipantWords {
    let observation = participant.observation();
    if let Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait) = participant.mechanism() {
        return ParticipantWords {
            title: observation.granted_kibibytes.as_ref().map_or_else(
                || t!("view.request_memory.output_wait.title"),
                |value| t!("view.request_memory.output_wait.title.amount", granted = format::digits(value.clone())),
            ),
            explanation: t!("view.request_memory.output_wait.explanation"),
            investigation: t!("view.request_memory.output_wait.investigation"),
            verify: t!("view.request_memory.output_wait.verify"),
        };
    }
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
    ParticipantWords { title, explanation, investigation, verify: t!("view.request_memory.verify") }
}

fn render(participant: Participant<'_>, resources: &[MemoryGrantAdmission]) -> MemoryParticipantExplanation {
    let observation = participant.observation();
    let words = participant_words(&participant);
    let mut details = vec![words.explanation, request_context(&participant)];
    let admission = participant.resource().and_then(|identity| {
        resources.binary_search_by(|group| group.resource.cmp(identity)).ok().map(|index| &resources[index])
    });
    if let Some(resource) = admission {
        let other = resource.participants.len().saturating_sub(1);
        details.push(t!("view.request_memory.admission.participants", n = other, count = format::count(other as f64)));
    }
    queue_details(observation, &mut details);
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
        observation_key: participant.key().to_owned(),
        observation_identity: observation.identity.clone(),
        mechanism: participant.mechanism(),
        qualified_request: participant.qualified_request().cloned(),
        title: words.title,
        explanation: details.join(" "),
        investigation: words.investigation,
        verify: words.verify,
        grant_admission: admission.map(|resource| resource.resource.clone()),
    }
}

fn queue_details(observation: &ExecutionGrantObservation, details: &mut Vec<String>) {
    if observation.state != ExecutionGrantState::Waiting {
        return;
    }
    details.push(match observation.queue.next_candidate {
        Some(true) => t!("view.request_memory.queue.next"),
        Some(false) => t!("view.request_memory.queue.not_next"),
        None => t!("view.request_memory.queue.unknown"),
    });
    if let Some(position) = &observation.queue.position {
        details.push(t!("view.request_memory.queue.position", position = format::digits(position.clone())));
    }
    if let Some(small) = observation.queue.small {
        details.push(if small {
            t!("view.request_memory.queue.small")
        } else {
            t!("view.request_memory.queue.regular")
        });
    }
}

fn request_context(participant: &Participant<'_>) -> String {
    let observation = participant.observation();
    match observation.request_association {
        RequestGrantAssociation::GenerationChecked if participant.qualified_request().is_none() => {
            t!("view.request_memory.request.unqualified")
        }
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

#[cfg(test)]
pub(crate) mod tests;
