//! Current execution reservations identify participants, never the statement that acquired them or a causal owner.
use super::memory_participants;
use crate::support::request_memory::{
    AdmissionShortfall, Participant, has_shared_admission_scope, qualified_participants,
};
use culprit_core::{
    Engine, MemoryGrantAdmission, MemoryParticipantExplanation, MemoryParticipantMechanism, MemoryParticipantReference,
    MemoryParticipantsReport, Result, SourceOutcome, format, t,
    telemetry::{
        ExecutionGrantObservation, ExecutionGrantState, GrantStatementRelationship, NativeIdentity, NativeReadContext,
        RequestGrantAssociation,
    },
};
use std::collections::BTreeMap;
mod maintenance;
mod reservation_demand;
mod workload_group_tempdb;

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
    let resources = admission_resources(&evidence, &cases, source);
    let mut limitations = limitations(&evidence, source);
    if resources.iter().any(|resource| resource.demand.is_some()) {
        limitations.push(t!("view.request_memory.demand.limit"));
    }
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
        limitations,
        entries: cases
            .into_iter()
            .map(|participant| render(participant, &resources, evidence.context.target.engine()))
            .collect(),
        admission_resources: resources.into_iter().map(|resource| resource.admission).collect(),
    })
}

struct AdmissionResource {
    admission: MemoryGrantAdmission,
    demand: Option<reservation_demand::ReservationDemand>,
    rebuild_reservations: maintenance::RebuildReservations,
}

fn admission_resources(
    evidence: &RequestMemoryEvidence<'_>,
    participants: &[Participant<'_>],
    source: Option<&SourceOutcome>,
) -> Vec<AdmissionResource> {
    if !evidence.context.has_qualified_local_owner() {
        return vec![];
    }
    let mut resources: BTreeMap<NativeIdentity, Vec<&Participant<'_>>> = BTreeMap::new();
    for participant in participants {
        if let Some(resource) = participant.resource() {
            resources.entry(resource.clone()).or_default().push(participant);
        }
    }
    resources
        .into_iter()
        .map(|(resource, members)| AdmissionResource {
            demand: reservation_demand::ReservationDemand::from_participants(&members, source),
            rebuild_reservations: maintenance::RebuildReservations::from_participants(
                &members,
                evidence.context.target.engine(),
            ),
            admission: MemoryGrantAdmission {
                resource,
                participants: members
                    .into_iter()
                    .map(|participant| MemoryParticipantReference {
                        observation_key: participant.key().to_owned(),
                        observation_identity: participant.observation().identity.clone(),
                    })
                    .collect(),
            },
        })
        .collect()
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
    if !has_shared_admission_scope(evidence.context)
        || observations.is_empty()
        || observations.iter().any(|observation| observation.resource_observation.is_none())
    {
        limits.push(t!("view.request_memory.resource.unqualified"));
    }
    limits.extend(memory_participants::source_limitation(source));
    if observations.iter().any(|observation| observation.workload_group_tempdb.is_some()) {
        limits.push(t!("view.request_memory.tempdb.limit"));
    }
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

fn participant_words(
    participant: &Participant<'_>,
    demand: Option<&reservation_demand::ReservationDemand>,
) -> ParticipantWords {
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
    let mut words = ParticipantWords { title, explanation, investigation, verify: t!("view.request_memory.verify") };
    if let Some(relation) = participant.admission_relation() {
        let detail = match relation.shortfall {
            AdmissionShortfall::RequiredAndRequested => t!(
                "view.request_memory.resource.below_required",
                available = format::digits(relation.available.into()),
                required = format::digits(relation.required.into()),
                requested = format::digits(relation.requested.into())
            ),
            AdmissionShortfall::Requested => t!(
                "view.request_memory.resource.below_requested",
                available = format::digits(relation.available.into()),
                required = format::digits(relation.required.into()),
                requested = format::digits(relation.requested.into())
            ),
        };
        words.explanation = format!("{} {detail} {}", words.explanation, t!("view.request_memory.resource.limit"));
        words.investigation = t!("view.request_memory.resource.investigation");
        words.verify = t!("view.request_memory.resource.verify");
        if let Some(demand) = demand {
            words.explanation = format!("{} {}", demand.explanation(), words.explanation);
            words.investigation = format!("{} {}", demand.investigation(), words.investigation);
        }
    }
    words
}

fn render(
    participant: Participant<'_>,
    resources: &[AdmissionResource],
    engine: Engine,
) -> MemoryParticipantExplanation {
    let observation = participant.observation();
    let admission = participant.resource().and_then(|identity| {
        resources.binary_search_by(|group| group.admission.resource.cmp(identity)).ok().map(|index| &resources[index])
    });
    let mut words = participant_words(&participant, admission.and_then(|resource| resource.demand.as_ref()));
    maintenance::append_context(
        &participant,
        admission.map(|resource| &resource.rebuild_reservations),
        engine,
        &mut words,
    );
    let mut details = vec![words.explanation, request_context(&participant)];
    if let Some(group) = workload_group_tempdb::explain(&participant) {
        details.push(group);
        words.investigation = format!("{} {}", words.investigation, t!("view.request_memory.tempdb.investigation"));
        words.verify = format!("{} {}", words.verify, t!("view.request_memory.tempdb.verify"));
    }
    if let Some(resource) = admission {
        let other = resource.admission.participants.len().saturating_sub(1);
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
        grant_admission: admission.map(|resource| resource.admission.resource.clone()),
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
