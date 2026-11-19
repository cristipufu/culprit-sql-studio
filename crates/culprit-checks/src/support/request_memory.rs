//! One native read qualifies execution-reservation participants and their observed memory mechanisms.
use super::source_coverage;
use culprit_core::{
    Engine, Error, MemoryParticipantMechanism, Result, SourceOutcome, t,
    telemetry::{
        ActivityEvidence, ActivityKey, ExecutionGrantObservation, ExecutionGrantState, GrantStatementRelationship,
        MAX_REQUEST_MEMORY_OBSERVATIONS, NativeIdentity, NativeReadContext, ObservationBounds, SharedResourceKind,
        SourcePartition, WaitMechanism, canonical_time,
    },
};
use std::collections::BTreeSet;

pub(crate) struct Participant<'a> {
    observation: &'a ExecutionGrantObservation,
    key: String,
    resource: Option<&'a NativeIdentity>,
    mechanism: Option<MemoryParticipantMechanism>,
    qualified_request: Option<&'a ActivityKey>,
    shared_admission_scope: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum AdmissionShortfall {
    RequiredAndRequested,
    Requested,
}

pub(crate) struct AdmissionRelation<'a> {
    pub(crate) shortfall: AdmissionShortfall,
    pub(crate) available: &'a str,
    pub(crate) required: &'a str,
    pub(crate) requested: &'a str,
}

impl<'a> Participant<'a> {
    pub(crate) fn observation(&self) -> &'a ExecutionGrantObservation {
        self.observation
    }
    pub(crate) fn key(&self) -> &str {
        &self.key
    }
    pub(crate) fn request(&self) -> Option<&'a culprit_core::telemetry::ActivityObservation> {
        self.qualified_request.and(self.observation.request.as_ref())
    }
    pub(crate) fn current_statement(&self) -> Option<&'a culprit_core::telemetry::DefinitionPayload> {
        if self.observation.statement_relationship != GrantStatementRelationship::ObservedCurrentStatement {
            return None;
        }
        Some(&self.request()?.statement)
    }
    pub(crate) fn mechanism(&self) -> Option<MemoryParticipantMechanism> {
        self.mechanism
    }
    pub(crate) fn resource(&self) -> Option<&'a NativeIdentity> {
        self.resource
    }
    pub(crate) fn qualified_request(&self) -> Option<&'a ActivityKey> {
        self.qualified_request
    }
    pub(crate) fn has_shared_admission_scope(&self) -> bool {
        self.shared_admission_scope
    }
    pub(crate) fn admission_relation(&self) -> Option<AdmissionRelation<'a>> {
        self.qualified_request?;
        self.resource?;
        if !self.shared_admission_scope {
            return None;
        }
        let observation = self.observation;
        if observation.state != ExecutionGrantState::Waiting || observation.queue.next_candidate != Some(true) {
            return None;
        }
        if observation
            .granted_kibibytes
            .as_deref()
            .is_some_and(|value| value.parse::<u64>().is_ok_and(|value| value > 0))
        {
            return None;
        }
        let available = observation.resource_observation.as_ref()?.available_kibibytes.as_deref()?;
        let required = observation.required_kibibytes.as_deref()?;
        let requested = observation.requested_kibibytes.as_deref()?;
        let available_value = i128::from(available.parse::<i64>().ok()?);
        let required_value = i128::from(required.parse::<u64>().ok()?);
        let requested_value = i128::from(requested.parse::<u64>().ok()?);
        if required_value == 0 || requested_value < required_value {
            return None;
        }
        let shortfall = if available_value < required_value {
            AdmissionShortfall::RequiredAndRequested
        } else if available_value < requested_value {
            AdmissionShortfall::Requested
        } else {
            return None;
        };
        Some(AdmissionRelation { shortfall, available, required, requested })
    }
    pub(crate) fn shares_admission_resource(&self, other: &Self, database: &str) -> bool {
        self.qualified_request.is_some()
            && other.qualified_request.is_some()
            && self.observation.shares_admission_resource(other.observation, database)
    }
}

pub(crate) fn has_shared_admission_scope(context: &NativeReadContext) -> bool {
    context.has_qualified_local_owner()
        && matches!(&context.partition, SourcePartition::InstanceLocal { member: Some(member), .. }
            if member.kind == SharedResourceKind::Instance)
}

pub(crate) fn qualified_participants<'a>(
    context: &NativeReadContext,
    observations: &[&'a ExecutionGrantObservation],
    source: Option<&SourceOutcome>,
) -> Result<Vec<Participant<'a>>> {
    let Some(database) = context.target.database() else { return Err(invalid()) };
    if !context.is_valid()
        || !matches!(context.partition, SourcePartition::InstanceLocal { .. })
        || observations.len() > MAX_REQUEST_MEMORY_OBSERVATIONS
        || !source_coverage::consistent_source(source, observations.iter().map(|row| row.source.as_str()))
    {
        return Err(invalid());
    }
    let disqualified =
        ActivityEvidence::disqualified_session_generations(observations.iter().filter_map(|row| row.request.as_ref()))?;
    let mut keys = BTreeSet::new();
    let bounds = ObservationBounds::new(&context.observed_from, &context.observed_until).ok_or_else(invalid)?;
    let mut point = None;
    let shared_admission_scope = has_shared_admission_scope(context);
    observations
        .iter()
        .map(|observation| {
            let at = canonical_time(&observation.observed_at).map_err(|_| invalid())?;
            if !observation.is_valid()
                || !observation.fits_context(context)
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
            let qualified_request = observation
                .request
                .as_ref()
                .filter(|request| {
                    context.has_qualified_local_owner()
                        && observation.is_admission_participant(&database.database)
                        && !disqualified.contains(&request.key.session)
                })
                .map(|request| &request.key);
            let resource = qualified_request.and(observation.resource.as_ref());
            let mechanism = qualified_request.and_then(|_| observed_mechanism(observation, context.target.engine()));
            Ok(Participant { observation, key, resource, mechanism, qualified_request, shared_admission_scope })
        })
        .collect()
}

fn observed_mechanism(observation: &ExecutionGrantObservation, engine: Engine) -> Option<MemoryParticipantMechanism> {
    if observation.state != ExecutionGrantState::Granted
        || !observation
            .granted_kibibytes
            .as_deref()
            .is_some_and(|value| value.parse::<u64>().is_ok_and(|granted| granted > 0))
    {
        return None;
    }
    let native_wait = observation.request.as_ref()?.wait.as_ref()?.native_type.as_deref()?;
    (engine.spec().native_wait_mechanism(native_wait) == Some(WaitMechanism::ClientResultOutput))
        .then_some(MemoryParticipantMechanism::ReservationDuringClientOutputWait)
}

fn invalid() -> Error {
    Error::invalid(t!("view.request_memory.invalid_capture"))
}
