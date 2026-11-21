//! Original capture-route self-observation. These client phases establish no database/member or native cost.
use super::*;
use crate::ErrorCode;

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    reader: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(reader)
}

pub const MAX_READ_CONNECTION_ATTEMPTS: usize = 4;
pub const MAX_READ_CONNECTION_PHASES: usize = 12;
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ReadConnectionUse {
    Unknown,
    FreshConnection,
    AuthenticatedReuse,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ReadConnectionStage {
    Acquire,
    RouteSelection,
    Transport,
    /// The driver combines native pre-login, optional TLS and login; failures cannot isolate those steps.
    HandshakeAndLogin,
    SessionSetup,
    FirstQuery,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum ReadConnectionOutcome {
    Pending,
    Completed,
    Redirected,
    Failed {
        code: ErrorCode,
        #[serde(deserialize_with = "required_nullable")]
        native_code: Option<u32>,
    },
    Stopped {
        reason: CollectionStop,
    },
    /// The original observation ended without establishing native completion or a confirmed stop.
    Unconfirmed,
}
impl ReadConnectionOutcome {
    /// Preserve only original typed causes/codes; elapsed time or an error message never supplies a stop request.
    pub fn from_error(error: &crate::Error) -> Self {
        match error.detail {
            Some(crate::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed)) => Self::Unconfirmed,
            Some(crate::ErrorDetail::CollectionStopped(reason)) if ReadStopCount::valid_reason(reason) => {
                Self::Stopped { reason }
            }
            _ => Self::Failed { code: error.code, native_code: None },
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ReadConnectionPhase {
    pub stage: ReadConnectionStage,
    /// First-query identity only; never a SQL payload, parameter value or endpoint label.
    #[serde(deserialize_with = "required_nullable")]
    pub template_id: Option<String>,
    pub outcome: ReadConnectionOutcome,
    pub client_started_at: String,
    /// End of this observer's lifetime; an unconfirmed outcome is not native completion.
    #[serde(deserialize_with = "required_nullable")]
    pub client_finished_at: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub client_elapsed_ms: Option<f64>,
}
impl ReadConnectionPhase {
    pub fn is_valid(&self) -> bool {
        let ended = self.client_finished_at.as_ref().is_some_and(|at| at.len() <= 64 && canonical_time(at).is_ok());
        let elapsed = self.client_elapsed_ms.is_some_and(|value| value.is_finite() && value >= 0.0);
        self.client_started_at.len() <= 64
            && canonical_time(&self.client_started_at).is_ok()
            && match self.stage {
                ReadConnectionStage::FirstQuery => self.template_id.as_deref().is_some_and(valid_digest),
                _ => self.template_id.is_none(),
            }
            && match &self.outcome {
                ReadConnectionOutcome::Pending => self.client_finished_at.is_none() && self.client_elapsed_ms.is_none(),
                ReadConnectionOutcome::Redirected => {
                    self.stage == ReadConnectionStage::HandshakeAndLogin && ended && elapsed
                }
                ReadConnectionOutcome::Failed { native_code, .. } => {
                    native_code.is_none_or(|code| code > 0) && ended && elapsed
                }
                ReadConnectionOutcome::Stopped { reason } => ReadStopCount::valid_reason(*reason) && ended && elapsed,
                ReadConnectionOutcome::Completed | ReadConnectionOutcome::Unconfirmed => ended && elapsed,
            }
    }
    pub fn has_ordered_client_clocks(&self) -> bool {
        self.client_finished_at
            .as_ref()
            .is_none_or(|until| ObservationBounds::new(&self.client_started_at, until).is_some())
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ReadConnectionAttempt {
    pub ordinal: u32,
    pub connection_use: ReadConnectionUse,
    /// Held reader generation only after actual acquisition. Never a physical/native database identity.
    #[serde(deserialize_with = "required_nullable")]
    pub reader_generation: Option<String>,
    pub phases: Vec<ReadConnectionPhase>,
    pub omitted_phases: u32,
}
impl ReadConnectionAttempt {
    pub fn is_valid(&self) -> bool {
        self.ordinal > 0
            && self.phases.len() <= MAX_READ_CONNECTION_PHASES
            && self
                .reader_generation
                .as_deref()
                .is_none_or(|value| value.parse::<u64>().is_ok_and(|n| n > 0 && n.to_string() == value))
            && self.phases.first().is_some_and(|phase| phase.stage == ReadConnectionStage::Acquire)
            && self.phases.iter().all(ReadConnectionPhase::is_valid)
            && self.phases.iter().filter(|phase| phase.stage == ReadConnectionStage::Acquire).count() == 1
            && self.phases.iter().filter(|phase| phase.stage == ReadConnectionStage::FirstQuery).count() <= 1
            && self.phases.iter().filter(|phase| phase.stage == ReadConnectionStage::HandshakeAndLogin).count() <= 2
            && self.phases.iter().filter(|phase| phase.stage == ReadConnectionStage::Transport).count() <= 2
            && (self.connection_use != ReadConnectionUse::AuthenticatedReuse
                || self
                    .phases
                    .iter()
                    .all(|phase| matches!(phase.stage, ReadConnectionStage::Acquire | ReadConnectionStage::FirstQuery)))
            && self.phases.iter().filter(|phase| phase.stage == ReadConnectionStage::FirstQuery).all(|_| {
                self.reader_generation.is_some()
                    && self.phases.first().is_some_and(|phase| phase.outcome == ReadConnectionOutcome::Completed)
            })
    }
}
impl ReadConnectionAttempt {
    fn follows(&self, before: &Self) -> bool {
        let acquiring = before.phases.first().is_some_and(|phase| phase.outcome == ReadConnectionOutcome::Pending);
        self.ordinal == before.ordinal
            && self.omitted_phases >= before.omitted_phases
            && (self.connection_use == before.connection_use
                || (acquiring && before.connection_use == ReadConnectionUse::Unknown))
            && (self.reader_generation == before.reader_generation || (acquiring && before.reader_generation.is_none()))
            && self.phases.len() >= before.phases.len()
            && before.phases.iter().zip(&self.phases).all(|(before, after)| after.follows(before))
    }
}
impl ReadConnectionPhase {
    fn follows(&self, before: &Self) -> bool {
        if before.outcome != ReadConnectionOutcome::Pending {
            return self == before;
        }
        self.stage == before.stage
            && self.template_id == before.template_id
            && self.client_started_at == before.client_started_at
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ReadConnectionWork {
    pub attempted: u32,
    pub omitted_attempts: u32,
    pub omitted_phases: u32,
    pub active_phases: u32,
    pub unconfirmed_phases: u32,
    pub attempts: Vec<ReadConnectionAttempt>,
}
impl ReadConnectionWork {
    pub fn is_valid(&self) -> bool {
        self.attempts.len() <= MAX_READ_CONNECTION_ATTEMPTS
            && u64::from(self.attempted) == self.attempts.len() as u64 + u64::from(self.omitted_attempts)
            && self
                .attempts
                .iter()
                .enumerate()
                .all(|(index, attempt)| attempt.ordinal as usize == index + 1 && attempt.is_valid())
            && self.attempts.iter().map(|attempt| u64::from(attempt.omitted_phases)).sum::<u64>()
                <= u64::from(self.omitted_phases)
            && self
                .attempts
                .iter()
                .flat_map(|attempt| &attempt.phases)
                .filter(|phase| phase.outcome == ReadConnectionOutcome::Pending)
                .count() as u64
                <= u64::from(self.active_phases)
            && self
                .attempts
                .iter()
                .flat_map(|attempt| &attempt.phases)
                .filter(|phase| phase.outcome == ReadConnectionOutcome::Unconfirmed)
                .count() as u64
                <= u64::from(self.unconfirmed_phases)
    }
    /// Absolute snapshots from one original recorder may append phases/attempts and finish pending phases.
    /// A completed original phase, acquired generation or known connection-use classification never changes.
    pub fn follows(&self, before: &Self) -> bool {
        self.is_valid()
            && before.is_valid()
            && self.attempted >= before.attempted
            && self.omitted_attempts >= before.omitted_attempts
            && self.omitted_phases >= before.omitted_phases
            && self.unconfirmed_phases >= before.unconfirmed_phases
            && self.attempts.len() >= before.attempts.len()
            && before.attempts.iter().zip(&self.attempts).all(|(before, after)| after.follows(before))
    }
    pub fn has_unconfirmed_work(&self) -> bool {
        self.active_phases > 0 || self.unconfirmed_phases > 0
    }
    pub fn has_complete_observation_roster(&self) -> bool {
        self.omitted_attempts == 0 && self.omitted_phases == 0 && self.active_phases == 0
    }
}
#[cfg(test)]
mod tests;
