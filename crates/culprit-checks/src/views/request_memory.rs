//! Current execution reservations identify participants, never the statement that acquired them or a causal owner.
use super::{memory_participants, source_coverage};
use culprit_core::{
    Error, MemoryGrantAdmission, MemoryParticipantExplanation, MemoryParticipantReference, MemoryParticipantsReport,
    Result, SourceOutcome, format, t,
    telemetry::{
        ExecutionGrantObservation, ExecutionGrantState, GrantStatementRelationship, MAX_REQUEST_MEMORY_OBSERVATIONS,
        NativeIdentity, NativeReadContext, ObservationBounds, RequestGrantAssociation, SourcePartition, canonical_time,
    },
};
use std::collections::{BTreeMap, BTreeSet};

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
    let mut cases = validate(&evidence, source)?;
    cases.sort_by(|left, right| {
        order(left.observation.state).cmp(&order(right.observation.state)).then_with(|| left.key.cmp(&right.key))
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

struct Participant<'a> {
    observation: &'a ExecutionGrantObservation,
    key: String,
    resource: Option<&'a NativeIdentity>,
}

fn validate<'a>(evidence: &RequestMemoryEvidence<'a>, source: Option<&SourceOutcome>) -> Result<Vec<Participant<'a>>> {
    let observations = evidence.observations;
    let Some(database) = evidence.context.target.database() else { return Err(invalid()) };
    if !evidence.context.is_valid()
        || !matches!(evidence.context.partition, SourcePartition::InstanceLocal { .. })
        || observations.len() > MAX_REQUEST_MEMORY_OBSERVATIONS
        || !source_coverage::consistent_source(source, observations.iter().map(|row| row.source.as_str()))
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    let bounds = ObservationBounds::new(&evidence.context.observed_from, &evidence.context.observed_until)
        .ok_or_else(invalid)?;
    let mut point = None;
    observations
        .iter()
        .map(|observation| {
            let at = canonical_time(&observation.observed_at).map_err(|_| invalid())?;
            if !observation.is_valid()
                || !observation.fits_database_capture(&database.database)
                || !bounds.contains(&observation.observed_at)
                || point.as_ref().is_some_and(|previous| previous != &at)
            {
                return Err(invalid());
            }
            point = Some(at);
            let key = observation.key()?;
            if !keys.insert(key.clone()) {
                return Err(invalid());
            }
            let resource = (evidence.context.has_qualified_local_owner()
                && observation.is_admission_participant(&database.database))
            .then_some(observation.resource.as_ref())
            .flatten();
            Ok(Participant { observation, key, resource })
        })
        .collect()
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
        let row = participant.observation;
        if let Some(resource) = participant.resource {
            resources.entry(resource.clone()).or_default().push(MemoryParticipantReference {
                observation_key: participant.key.clone(),
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

fn render(participant: Participant<'_>, resources: &[MemoryGrantAdmission]) -> MemoryParticipantExplanation {
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
    let admission = participant.resource.and_then(|identity| {
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
        observation_key: participant.key,
        observation_identity: observation.identity.clone(),
        title,
        explanation: details.join(" "),
        investigation,
        verify: t!("view.request_memory.verify"),
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
