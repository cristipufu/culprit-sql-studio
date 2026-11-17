//! One request-bound native point shares original-source, membership and read-coverage rules across readers.
use super::*;

pub const MAX_FOCUSED_REQUEST_OBSERVATIONS: usize = 1;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FocusedRequestBinding {
    Independent,
    Matched,
    UnverifiedOriginalScope,
    ParticipantMissing,
    ParticipantChanged,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FocusedRequestMembership {
    Observed,
    NotObserved,
    Unavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FocusedRequestResult {
    /// Retained once in the source descriptor. It is absent for an independent live point.
    pub focus: Option<Box<ActivityRequestFocus>>,
    /// Native role observed by this read, not the original role, login permissions or configured routing intent.
    pub database_writable: Option<bool>,
    pub binding: FocusedRequestBinding,
    pub membership: FocusedRequestMembership,
}

impl FocusedRequestResult {
    pub fn is_valid_header(&self, snapshot: &StateSnapshotDescription) -> bool {
        snapshot.is_valid()
            && matches!(snapshot.dataset, SnapshotDataset::MaintenanceProgress | SnapshotDataset::CurrentRequestWork)
            && snapshot.sources.len() == 1
            && snapshot.records as usize <= MAX_FOCUSED_REQUEST_OBSERVATIONS
            && match &self.focus {
                None => self.binding == FocusedRequestBinding::Independent,
                Some(focus) => {
                    self.binding != FocusedRequestBinding::Independent
                        && focus.is_valid()
                        && ObservationBounds::new(&focus.original_observed_at, &snapshot.observed_until).is_some()
                }
            }
            && match self.membership {
                FocusedRequestMembership::Observed => {
                    snapshot.records == 1
                        && !matches!(
                            self.binding,
                            FocusedRequestBinding::ParticipantMissing | FocusedRequestBinding::ParticipantChanged
                        )
                }
                FocusedRequestMembership::NotObserved => snapshot.records == 0,
                FocusedRequestMembership::Unavailable => {
                    snapshot.records == 0
                        && matches!(
                            snapshot.sources[0].state,
                            crate::SourceState::Partial
                                | crate::SourceState::Unavailable
                                | crate::SourceState::NotApplicable
                        )
                }
            }
    }

    /// Partition consistency delegates here; do not recurse through NativeReadContext::from_snapshot.
    pub fn is_valid_source(&self, source: &CollectionDescription, snapshot: &StateSnapshotDescription) -> bool {
        let expected = match &source.observation {
            SourceObservation::MaintenanceProgress { .. } => SnapshotDataset::MaintenanceProgress,
            SourceObservation::CurrentRequestWork { profile, .. } => {
                if source.target.engine() != profile.engine()
                    || self.focus.as_deref().is_some_and(|focus| !profile.accepts_focus(focus))
                {
                    return false;
                }
                SnapshotDataset::CurrentRequestWork
            }
            _ => return false,
        };
        snapshot.dataset == expected
            && self.is_valid_header(snapshot)
            && self.is_valid_for(&self.read_context(source, snapshot))
            && snapshot
                .sources
                .iter()
                .all(|coverage| focused_request_coverage_fits_context(&self.read_context(source, snapshot), coverage))
    }

    pub fn read_context(
        &self,
        source: &CollectionDescription,
        snapshot: &StateSnapshotDescription,
    ) -> NativeReadContext {
        NativeReadContext {
            target: source.target.clone(),
            partition: source.partition.clone(),
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
            database_writable: self.database_writable,
        }
    }

    pub fn is_valid_for(&self, context: &NativeReadContext) -> bool {
        if !context.is_valid()
            || context.target.database().is_none()
            || self.database_writable != context.database_writable
        {
            return false;
        }
        match &self.focus {
            None => self.binding == FocusedRequestBinding::Independent,
            Some(focus) => {
                focus.is_valid()
                    && !focused_request_scope_changed(focus, context)
                    && ObservationBounds::new(&focus.original_observed_at, &context.observed_until).is_some()
                    && match self.binding {
                        FocusedRequestBinding::Independent => false,
                        FocusedRequestBinding::UnverifiedOriginalScope => {
                            !context.has_qualified_local_owner() || focus.partition == SourcePartition::Target
                        }
                        _ => original_scope_matches(focus, context),
                    }
            }
        }
    }

    /// The same original request and statement must own the point; a current reader supplies no missing origin.
    pub fn matches_point(
        &self,
        context: &NativeReadContext,
        actor: &ActivityKey,
        statement: Option<&NativeIdentity>,
        observed_at: &str,
    ) -> bool {
        self.membership == FocusedRequestMembership::Observed
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(observed_at))
            && self.focus.as_ref().is_none_or(|focus| {
                actor == &focus.original.key
                    && statement == Some(&focus.statement_identity)
                    && ObservationBounds::new(&focus.original_observed_at, observed_at).is_some()
            })
    }

    pub fn original_reference(&self) -> Option<&DiagnosticEvidenceRef> {
        self.focus.as_ref().map(|focus| &focus.parent)
    }

    pub fn expected_actor(&self) -> Option<&ActivityKey> {
        self.focus.as_ref().map(|focus| &focus.original.key)
    }
}

/// The current read's native owner never fills a missing original physical member.
pub fn focused_request_original_scope_matches(focus: &ActivityRequestFocus, context: &NativeReadContext) -> bool {
    focus.is_valid() && original_scope_matches(focus, context)
}

/// Missing native facts remain unknown; any independently observed conflict still prevents reuse of the origin.
pub fn focused_request_scope_changed(focus: &ActivityRequestFocus, context: &NativeReadContext) -> bool {
    focus.target.telemetry_id().ok() != context.target.telemetry_id().ok()
        || matches!((focus.database_writable, context.database_writable), (Some(before), Some(after)) if before != after)
        || match (&focus.partition, &context.partition) {
            (
                SourcePartition::InstanceLocal { member: before_member, epoch: before_epoch },
                SourcePartition::InstanceLocal { member: after_member, epoch: after_epoch },
            ) => {
                matches!((before_member, after_member), (Some(before), Some(after)) if before != after)
                    || matches!((before_epoch, after_epoch), (Some(before), Some(after)) if before != after)
            }
            _ => false,
        }
}

fn original_scope_matches(focus: &ActivityRequestFocus, context: &NativeReadContext) -> bool {
    context.has_qualified_local_owner()
        && matches!(&focus.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) })
        && focus.target.telemetry_id().ok() == context.target.telemetry_id().ok()
        && focus.partition == context.partition
        && focus.database_writable == context.database_writable
}

pub(crate) fn focused_request_coverage_fits_context(
    context: &NativeReadContext,
    coverage: &StateSourceCoverage,
) -> bool {
    context.has_qualified_local_owner() || !coverage.complete_scope
}
