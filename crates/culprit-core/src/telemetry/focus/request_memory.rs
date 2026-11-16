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
    /// Exact original native grant; other rows cannot replace the retained activity participant.
    pub subject: Option<NativeIdentity>,
    /// Resource reported by that subject in this read. A missing/masked identity cannot qualify peers.
    pub resource: Option<NativeIdentity>,
    /// Exact native roster only; original generations, tokens and metrics remain in authenticated raw rows.
    pub peers: Vec<NativeIdentity>,
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
        let mut identities = std::collections::BTreeSet::new();
        self.focus.is_request_memory_focus()
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
            && (self.source != FocusSourceState::Matched
                || ObservationBounds::new(&self.focus.original_observed_at, &self.observed_from).is_some())
            && (self.source == FocusSourceState::Matched || self.membership == RequestMemoryMembership::Unverified)
            && self.subject.as_ref().is_none_or(NativeIdentity::is_valid)
            && self.resource.as_ref().is_none_or(NativeIdentity::is_valid)
            && (self.membership == RequestMemoryMembership::Observed) == self.subject.is_some()
            && (self.membership == RequestMemoryMembership::Observed
                || self.resource.is_none() && self.peers.is_empty())
            && (self.resource.is_some() || self.peers.is_empty())
            && self.peers.len() < MAX_REQUEST_MEMORY_OBSERVATIONS
            && self.peers.iter().all(|identity| {
                identity.is_valid() && Some(identity) != self.subject.as_ref() && identities.insert(identity)
            })
    }
    fn accepts_participant(&self, row: &ExecutionGrantObservation) -> bool {
        self.is_valid()
            && self.source == FocusSourceState::Matched
            && self.membership == RequestMemoryMembership::Observed
            && self.focus.target.database().is_some_and(|database| row.is_admission_participant(&database.database))
            && row.resource == self.resource
            && ObservationBounds::new(&self.observed_from, &self.observed_until)
                .is_some_and(|bounds| bounds.contains(&row.observed_at))
    }
    pub fn is_subject(&self, row: &ExecutionGrantObservation) -> bool {
        self.accepts_participant(row)
            && Some(&row.identity) == self.subject.as_ref()
            && row.request.as_ref().is_some_and(|request| {
                request.key == self.focus.original.key
                    && request.statement_identity.as_ref() == Some(&self.focus.statement_identity)
            })
    }
    pub fn is_peer(&self, row: &ExecutionGrantObservation) -> bool {
        self.accepts_participant(row)
            && self.resource.is_some()
            && self.peers.contains(&row.identity)
            && row.request.as_ref().is_some_and(|request| request.key != self.focus.original.key)
    }
    pub fn accepts_observation(&self, row: &ExecutionGrantObservation) -> bool {
        self.is_subject(row) || self.is_peer(row)
    }
    /// Full native input closure; partial archive reads remain partial and cannot settle incident analysis.
    pub fn matches_observations<'a>(&self, rows: impl IntoIterator<Item = &'a ExecutionGrantObservation>) -> bool {
        let mut subject = 0usize;
        let mut peers = std::collections::BTreeSet::new();
        let mut point = None;
        let mut source = None;
        for row in rows {
            let Ok(observed_at) = canonical_time(&row.observed_at) else { return false };
            if point.as_ref().is_some_and(|at| at != &observed_at)
                || source.is_some_and(|source| source != row.source.as_str())
                || !self.accepts_observation(row)
            {
                return false;
            }
            point = Some(observed_at);
            source = Some(row.source.as_str());
            if self.is_subject(row) {
                subject += 1;
            } else if !peers.insert(&row.identity) {
                return false;
            }
        }
        self.is_valid()
            && subject == usize::from(self.membership == RequestMemoryMembership::Observed)
            && peers.len() == self.peers.len()
    }
}
