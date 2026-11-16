//! A focused lock closure retains its current frame without upgrading the original participant's source proof.
use super::*;
use std::collections::BTreeSet;

pub const MAX_BLOCKING_LOOKUPS: u32 = 64;
pub const MAX_BLOCKING_DEPTH: u32 = 8;
pub const MAX_BLOCKING_PARTICIPANTS: u32 = MAX_BLOCKING_LOOKUPS + 1;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BlockingClosureStatus {
    NotRead,
    /// The bounded traversal finished. Hidden/ended owner contexts can still remain unresolved.
    Observed,
    LookupLimit,
    DepthLimit,
    ParticipantLimit,
}
impl BlockingClosureStatus {
    pub fn is_limited(self) -> bool {
        matches!(self, Self::LookupLimit | Self::DepthLimit | Self::ParticipantLimit)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct BlockingClosureDescription {
    pub status: BlockingClosureStatus,
    pub max_lookups: u32,
    pub max_depth: u32,
    pub max_participants: u32,
}

impl BlockingClosureDescription {
    pub fn is_valid(&self) -> bool {
        (1..=MAX_BLOCKING_LOOKUPS).contains(&self.max_lookups)
            && (1..=MAX_BLOCKING_DEPTH).contains(&self.max_depth)
            && (1..=MAX_BLOCKING_PARTICIPANTS).contains(&self.max_participants)
    }

    pub fn is_limited(&self) -> bool {
        self.status.is_limited()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityBlockingFocusResult {
    pub focus: ActivityRequestFocus,
    pub binding: FocusedRequestBinding,
    pub membership: FocusedRequestMembership,
    /// Final request guard, separate from the earlier native point and from incident recovery.
    pub request_end: BlockingRequestState,
    pub closure: BlockingClosureDescription,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BlockingRequestState {
    NotRead,
    SameStatement,
    Changed,
    Missing,
}

impl ActivityRequestFocus {
    pub fn is_blocking_focus(&self) -> bool {
        self.is_valid() && self.original.waiting_on_lock == Some(true)
    }
}

impl ActivityBlockingFocusResult {
    pub fn is_valid_header(&self, snapshot: &ActivitySnapshotDescription) -> bool {
        self.focus.is_blocking_focus()
            && snapshot.is_valid()
            && !snapshot.coverage.complete_scope
            && self.closure.is_valid()
            && snapshot.observations <= self.closure.max_participants
            && self.binding != FocusedRequestBinding::Independent
            && ObservationBounds::new(&self.focus.original_observed_at, &snapshot.observed_from).is_some()
            && match self.membership {
                FocusedRequestMembership::Observed => {
                    snapshot.observations > 0
                        && self.request_end != BlockingRequestState::NotRead
                        && matches!(
                            self.binding,
                            FocusedRequestBinding::Matched | FocusedRequestBinding::UnverifiedOriginalScope
                        )
                }
                FocusedRequestMembership::NotObserved | FocusedRequestMembership::Unavailable => {
                    snapshot.observations == 0
                        && self.closure.status == BlockingClosureStatus::NotRead
                        && self.request_end == BlockingRequestState::NotRead
                }
            }
            && (!matches!(
                self.binding,
                FocusedRequestBinding::ParticipantMissing | FocusedRequestBinding::ParticipantChanged
            ) || self.membership == FocusedRequestMembership::NotObserved)
    }

    pub fn context(&self, source: &CollectionDescription, snapshot: &ActivitySnapshotDescription) -> NativeReadContext {
        NativeReadContext {
            target: source.target.clone(),
            partition: source.partition.clone(),
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
            database_writable: snapshot.database_writable,
        }
    }

    /// Called from partition validation: do not recurse through the source's public validation methods.
    pub fn is_valid_source(&self, source: &CollectionDescription, snapshot: &ActivitySnapshotDescription) -> bool {
        let context = self.context(source, snapshot);
        self.is_valid_header(snapshot)
            && context.is_valid()
            && !focused_request_scope_changed(&self.focus, &context)
            && match self.binding {
                FocusedRequestBinding::Independent => false,
                FocusedRequestBinding::UnverifiedOriginalScope => {
                    self.focus.partition == SourcePartition::Target || !context.has_qualified_local_owner()
                }
                _ => focused_request_original_scope_matches(&self.focus, &context),
            }
    }

    pub fn accepts_row(&self, row: &ActivityObservation) -> bool {
        row.is_valid()
            && self.membership == FocusedRequestMembership::Observed
            && if row.key == self.focus.original.key {
                row.statement_identity.as_ref() == Some(&self.focus.statement_identity)
                    && self.focus.target.database().is_some_and(|database| {
                        matches!(row.scope, ActivityScopeBasis::RequestDatabase | ActivityScopeBasis::SessionDatabase)
                            && row.database.as_deref() == Some(database.database.as_str())
                    })
            } else {
                true
            }
    }

    /// Receipt verification stays with the archive owner. A complete frame cannot add unrelated actors or a new waiter.
    pub fn accepts_frame(&self, frame: &ActivityFrameEvidence<'_>) -> bool {
        if !self.is_valid_source(frame.source(), frame.native()) {
            return false;
        }
        let rows = frame.rows().map(|row| row.observation.clone()).collect::<Vec<_>>();
        if rows.iter().any(|row| !self.accepts_row(row)) {
            return false;
        }
        if !frame.read_complete() {
            return true;
        }
        match self.membership {
            FocusedRequestMembership::Observed => {
                frame.row(&self.focus.original.key).is_some()
                    && ActivityEvidence::context_for(&rows, &BTreeSet::from([self.focus.original.key.clone()]))
                        .is_ok_and(|reached| reached.len() == rows.len())
            }
            _ => rows.is_empty(),
        }
    }

    /// Completes the selected owner-context question, never a full lock population or SQL-acquisition history.
    pub fn has_complete_owner_context(&self, frame: &ActivityFrameEvidence<'_>) -> bool {
        if !frame.read_complete()
            || !self.accepts_frame(frame)
            || self.binding != FocusedRequestBinding::Matched
            || self.closure.status != BlockingClosureStatus::Observed
            || self.request_end != BlockingRequestState::SameStatement
        {
            return false;
        }
        let rows = frame.rows().map(|row| row.observation.clone()).collect::<Vec<_>>();
        ActivityEvidence::blocking_observations(&rows).is_ok_and(|edges| {
            !edges.is_empty() && edges.iter().all(|edge| edge.owner_observed && !edge.reports_waiter_session_id())
        })
    }
}

#[cfg(test)]
mod tests;
