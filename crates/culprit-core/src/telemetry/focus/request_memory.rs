//! A later execution-grant point can explain a retained request, never absence of that request or its cause.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RequestMemoryMembership {
    Observed,
    /// No matching qualified grant row. This grants-driven source does not prove request disappearance.
    NotObserved,
    Unverified,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityRequestMemoryFocusResult {
    pub focus: ActivityRequestFocus,
    pub observed_from: String,
    pub observed_until: String,
    pub source: FocusSourceState,
    pub membership: RequestMemoryMembership,
}

impl ActivityRequestFocus {
    /// A grant follow-up requires the original connected compute member and generation, not a target namespace.
    pub fn is_request_memory_focus(&self) -> bool {
        self.is_valid()
            && self.partition.native_epoch().is_some()
            && self.original.wait.as_ref().is_some_and(|wait| wait.class == Some(ActivityWaitClass::ExecutionGrant))
    }
}

impl ActivityRequestMemoryFocusResult {
    pub fn is_valid(&self) -> bool {
        self.focus.is_request_memory_focus()
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
            && (self.source != FocusSourceState::Matched
                || ObservationBounds::new(&self.focus.original_observed_at, &self.observed_from).is_some())
            && (self.source == FocusSourceState::Matched || self.membership == RequestMemoryMembership::Unverified)
    }
    pub fn accepts_observation(&self, row: &ExecutionGrantObservation) -> bool {
        self.is_valid()
            && self.source == FocusSourceState::Matched
            && self.membership == RequestMemoryMembership::Observed
            && row.is_valid()
            && row.request_association == RequestGrantAssociation::GenerationChecked
            && row.scope == Some(crate::MeasurementScope::Database)
            && self.focus.target.database().is_some_and(|database| row.fits_database_capture(&database.database))
            && ObservationBounds::new(&self.observed_from, &self.observed_until)
                .is_some_and(|bounds| bounds.contains(&row.observed_at))
            && row.request.as_ref().is_some_and(|request| {
                request.key == self.focus.original.key
                    && request.statement_identity.as_ref() == Some(&self.focus.statement_identity)
            })
    }
}
