//! Compare original positive reservations with one consistent native resource population, never inferred usage.
use crate::support::request_memory::Participant;
use culprit_core::{SourceOutcome, SourceState, format, t, telemetry::ExecutionGrantState};
use std::collections::BTreeSet;

pub(super) enum ReservationDemand {
    LargerReservation { native_count: u64, native_reserved: u64, largest: u64 },
    SeveralSmallerReservations { native_count: u64, native_reserved: u64, count: usize, smallest: u64, largest: u64 },
}

impl ReservationDemand {
    /// The caller groups the already-qualified original participants by their exact admission resource once.
    pub(super) fn from_participants(members: &[&Participant<'_>], source: Option<&SourceOutcome>) -> Option<Self> {
        if !matches!(source?.state, SourceState::Collected | SourceState::Partial) {
            return None;
        }
        let first = members.first()?;
        if !first.has_shared_admission_scope() {
            return None;
        }
        let native = first.observation().resource_observation.as_ref()?;
        let native_count = native.grantee_count.as_deref()?.parse::<u64>().ok()?;
        let native_reserved = native.granted_kibibytes.as_deref()?.parse::<u64>().ok()?;
        let native_waiting = native.waiter_count.as_deref()?.parse::<u64>().ok()?;
        if native_count == 0 || native_reserved == 0 || native_waiting == 0 {
            return None;
        }
        let mut requests = BTreeSet::new();
        let mut granted = Vec::new();
        let mut waiting = 0;
        let mut has_shortfall = false;
        for member in members {
            if member.observation().resource_observation.as_ref() != Some(native)
                || !requests.insert(member.qualified_request()?)
            {
                return None;
            }
            match member.observation().state {
                ExecutionGrantState::Waiting => {
                    waiting += 1;
                    has_shortfall |= member.admission_relation().is_some();
                }
                ExecutionGrantState::Granted => {
                    let amount = member.observation().granted_kibibytes.as_deref()?.parse::<u64>().ok()?;
                    if amount == 0 || amount > native_reserved {
                        return None;
                    }
                    granted.push(amount);
                }
            }
        }
        if !has_shortfall || native_count < granted.len() as u64 || native_waiting < waiting {
            return None;
        }
        let largest = *granted.iter().max()?;
        if u128::from(largest) * 2 > u128::from(native_reserved) {
            Some(Self::LargerReservation { native_count, native_reserved, largest })
        } else if granted.len() >= 2 {
            Some(Self::SeveralSmallerReservations {
                native_count,
                native_reserved,
                count: granted.len(),
                smallest: *granted.iter().min()?,
                largest,
            })
        } else {
            None
        }
    }

    pub(super) fn explanation(&self) -> String {
        match self {
            Self::LargerReservation { native_count, native_reserved, largest } => t!(
                "view.request_memory.demand.larger",
                native_count = format::digits(native_count.to_string()),
                native_reserved = format::digits(native_reserved.to_string()),
                largest = format::digits(largest.to_string())
            ),
            Self::SeveralSmallerReservations { native_count, native_reserved, count, smallest, largest } => t!(
                "view.request_memory.demand.several",
                native_count = format::digits(native_count.to_string()),
                native_reserved = format::digits(native_reserved.to_string()),
                n = *count,
                count = format::count(*count as f64),
                smallest = format::digits(smallest.to_string()),
                largest = format::digits(largest.to_string())
            ),
        }
    }

    pub(super) fn investigation(&self) -> String {
        match self {
            Self::LargerReservation { .. } => t!("view.request_memory.demand.larger_investigation"),
            Self::SeveralSmallerReservations { .. } => t!("view.request_memory.demand.several_investigation"),
        }
    }
}

#[cfg(test)]
mod tests;
