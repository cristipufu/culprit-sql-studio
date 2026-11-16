//! Compiler admission follow-ups name one affected native request, never a pool member or memory owner.
use super::*;
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CompilationAdmissionMembership {
    Observed,
    /// The original statement is not currently verified as waiting. This does not clear the original episode.
    NotObserved,
    Unverified,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityCompilationAdmissionFocusResult {
    pub focus: ActivityRequestFocus,
    pub observed_from: String,
    pub observed_until: String,
    pub source: FocusSourceState,
    pub membership: CompilationAdmissionMembership,
    /// Exact independently read gateway roster. Membership of the affected request in these pools is unknown.
    pub gateways: Vec<NativeIdentity>,
}

impl ActivityRequestFocus {
    pub fn is_compilation_admission_focus(&self) -> bool {
        self.is_valid()
            && self.original.scope == ActivityScopeBasis::RequestDatabase
            && self.partition.native_epoch().is_some()
            && self
                .original
                .wait
                .as_ref()
                .is_some_and(|wait| wait.class == Some(ActivityWaitClass::CompilationAdmission))
    }
}

impl ActivityCompilationAdmissionFocusResult {
    pub fn is_valid(&self) -> bool {
        let mut identities = BTreeSet::new();
        self.focus.is_compilation_admission_focus()
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
            && (self.source != FocusSourceState::Matched
                || ObservationBounds::new(&self.focus.original_observed_at, &self.observed_from).is_some())
            && (self.source == FocusSourceState::Matched
                || self.membership == CompilationAdmissionMembership::Unverified)
            && (self.membership == CompilationAdmissionMembership::Observed || self.gateways.is_empty())
            && self.gateways.len() <= MAX_FOCUSED_COMPILER_GATEWAYS
            && self.gateways.iter().all(|identity| identity.is_valid() && identities.insert(identity))
    }

    pub fn accepts_observation(&self, row: &OperationalObservation) -> bool {
        if !self.is_valid()
            || self.source != FocusSourceState::Matched
            || self.membership != CompilationAdmissionMembership::Observed
            || !row.is_valid()
            || row.source != crate::source_id::COMPILATION_GATEWAYS
        {
            return false;
        }
        match &row.fact {
            OperationalFact::CompilerWaiter(waiter) => {
                waiter.request.key == self.focus.original.key
                    && waiter.request.statement_identity.as_ref() == Some(&self.focus.statement_identity)
                    && self
                        .focus
                        .target
                        .database()
                        .is_some_and(|database| waiter.request.database_context_is_consistent(&database.database))
                    && ObservationBounds::new(&self.observed_from, &self.observed_until)
                        .is_some_and(|bounds| bounds.contains(&waiter.observed_at))
            }
            OperationalFact::CounterSet(counters) => {
                self.gateways.contains(&counters.identity) && CompilerGatewayFacts::from_observation(row).is_ok()
            }
            _ => false,
        }
    }

    pub fn matches_observations<'a>(&self, rows: impl IntoIterator<Item = &'a OperationalObservation>) -> bool {
        let mut waiter = 0usize;
        let mut gateways = BTreeSet::new();
        for row in rows {
            if !self.accepts_observation(row) {
                return false;
            }
            match &row.fact {
                OperationalFact::CompilerWaiter(_) => waiter += 1,
                OperationalFact::CounterSet(counters) => {
                    if !gateways.insert(&counters.identity) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        self.is_valid()
            && waiter == usize::from(self.membership == CompilationAdmissionMembership::Observed)
            && gateways.len() == self.gateways.len()
    }
}
