//! Qualified current maintenance syntax describes the observed request, never the statement that acquired memory.
use super::{Participant, ParticipantWords};
use culprit_core::{
    Engine, MaintenanceClassification, MaintenanceKind, classify_maintenance, t,
    telemetry::{ActivityKey, ExecutionGrantState},
};

pub(super) struct RebuildReservations(Vec<ActivityKey>);

impl RebuildReservations {
    /// Members already belong to one admitted native resource and observation point.
    pub(super) fn from_participants(participants: &[&Participant<'_>], engine: Engine) -> Self {
        Self(
            participants
                .iter()
                .filter(|participant| Role::of(participant, engine) == Some(Role::Reserved))
                .filter_map(|participant| participant.qualified_request().cloned())
                .collect(),
        )
    }

    fn has_other_request(&self, participant: &Participant<'_>) -> bool {
        participant.qualified_request().is_some_and(|request| self.0.iter().any(|reserved| reserved != request))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Reserved,
    Waiting,
}

impl Role {
    fn of(participant: &Participant<'_>, engine: Engine) -> Option<Self> {
        if !participant.has_shared_admission_scope() || participant.mechanism().is_some() {
            return None;
        }
        participant.resource()?;
        participant.current_statement()?;
        let request = participant.request()?;
        if classify_maintenance(engine, request)
            != (MaintenanceClassification::Maintenance { kind: MaintenanceKind::IndexRebuild })
        {
            return None;
        }
        match participant.observation().state {
            ExecutionGrantState::Waiting => Some(Self::Waiting),
            ExecutionGrantState::Granted
                if participant
                    .observation()
                    .granted_kibibytes
                    .as_deref()
                    .is_some_and(|value| value.parse::<u64>().is_ok_and(|value| value > 0)) =>
            {
                Some(Self::Reserved)
            }
            ExecutionGrantState::Granted => None,
        }
    }

    fn explanation(self) -> String {
        match self {
            Self::Reserved => t!("view.request_memory.maintenance.reserved"),
            Self::Waiting => t!("view.request_memory.maintenance.waiting"),
        }
    }
}

pub(super) fn append_context(
    participant: &Participant<'_>,
    reservations: Option<&RebuildReservations>,
    engine: Engine,
    words: &mut ParticipantWords,
) {
    // The existing observed output-wait mechanism keeps its own investigation and verification unchanged.
    if participant.mechanism().is_some() {
        return;
    }
    let role = Role::of(participant, engine);
    let other_reservation = participant.admission_relation().is_some()
        && reservations.is_some_and(|reservations| reservations.has_other_request(participant));
    if role.is_none() && !other_reservation {
        return;
    }
    if let Some(role) = role {
        words.explanation = format!("{} {}", words.explanation, role.explanation());
    }
    if other_reservation {
        words.explanation = format!("{} {}", words.explanation, t!("view.request_memory.maintenance.same_resource"));
    }
    words.investigation = format!("{} {}", words.investigation, t!("view.request_memory.maintenance.investigate"));
    words.verify = format!("{} {}", words.verify, t!("view.request_memory.maintenance.verify"));
}

#[cfg(test)]
mod tests;
