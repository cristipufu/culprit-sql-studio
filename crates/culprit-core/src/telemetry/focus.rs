//! A focused native lookup names one authenticated original participant, never a new broad population.
use super::*;
use crate::{ObservedWaitResource, WaitResourceKind};

mod compiler_admission;
mod current_request_work;
mod plan;
mod question;
mod request_memory;
pub use compiler_admission::{ActivityCompilationAdmissionFocusResult, CompilationAdmissionMembership};
pub use plan::{DiagnosticFocusPlan, PageWaitFocusPlan, plan_page_wait_writer_focus, supports_page_wait_writers};
pub use question::DiagnosticFocus;
pub use request_memory::{ActivityRequestMemoryFocusResult, RequestMemoryMembership};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityRequestFocus {
    pub parent: DiagnosticEvidenceRef,
    pub target: CollectionTarget,
    pub partition: SourcePartition,
    /// Observed database updateability at the parent point, not routing intent, permission or member identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub database_writable: Option<bool>,
    pub source: String,
    pub epoch: String,
    /// The original native observation clock, distinct from a later lookup's bounds.
    pub original_observed_at: String,
    /// Original SQL availability, counters and client context remain owned by this exact parent row.
    pub original: ActivityObservation,
    /// Required source-owned native statement token; a query hash or display SQL is insufficient.
    pub statement_identity: NativeIdentity,
}

impl ActivityRequestFocus {
    /// A shared follow-up needs the original native member; a database route cannot confer shared authority.
    pub fn is_valid_for(&self, subject: &CollectionSubject) -> bool {
        self.is_valid()
            && match subject {
                CollectionSubject::Database => true,
                CollectionSubject::Shared { resource } => matches!(
                    &self.partition,
                    SourcePartition::InstanceLocal { member: Some(member), .. } if member == resource
                ),
            }
    }

    /// Scope consistency only. The native reader still rechecks the original participant and both resource owners;
    /// the caller separately authenticates the original parent. Database and instance epochs have distinct meaning.
    pub fn matches_capture_scope(&self, source: &CollectionDescription) -> bool {
        if !self.is_valid() || !source.partition_is_consistent() {
            return false;
        }
        match &source.target {
            CollectionTarget::Database(_) => self.target == source.target && self.partition == source.partition,
            CollectionTarget::Shared(resource) => {
                self.is_valid_for(&CollectionSubject::Shared { resource: resource.clone() })
                    && source.is_partition_qualified()
                    && matches!(
                        &source.partition,
                        SourcePartition::InstanceLocal { member: Some(member), .. } if member == resource
                    )
            }
        }
    }

    /// The caller resolves the exact receipt through the outbox/recovery owner before invoking this comparison.
    pub fn matches_parent(
        &self,
        source: &CollectionDescription,
        original: &ActivityObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> bool {
        self.is_valid()
            && *reference == self.parent
            && *original == self.original
            && self.target.telemetry_id().ok() == source.target.telemetry_id().ok()
            && self.partition == source.partition
            && self.source == source.source
            && source.captured_range().is_ok_and(|range| range.scope_id == self.parent.source_scope_id)
            && source.observation.activity().is_some_and(|snapshot| {
                snapshot.is_valid()
                    && snapshot.epoch.as_deref() == Some(self.epoch.as_str())
                    && snapshot.database_writable == self.database_writable
                    && snapshot.at == self.original_observed_at
            })
    }

    pub fn is_valid(&self) -> bool {
        self.parent.is_valid()
            && self.parent.stream == TelemetryStream::ActivityObservations
            && matches!(&self.parent.native_key, NativeRecordIdentity::Activity(key) if *key == self.original.key)
            && self.partition.is_valid_for(&self.target)
            && self.partition.is_qualified()
            && match &self.partition {
                SourcePartition::Target => true,
                SourcePartition::InstanceLocal { epoch, .. } => epoch.as_deref() == Some(self.epoch.as_str()),
            }
            && self.target.database().is_some()
            && valid_key(&self.source)
            && valid_key(&self.epoch)
            && self.original_observed_at.len() <= 64
            && canonical_time(&self.original_observed_at).is_ok()
            && self.original.is_valid()
            && self.original.key.request.is_some()
            && matches!(self.original.scope, ActivityScopeBasis::RequestDatabase | ActivityScopeBasis::SessionDatabase)
            && self
                .target
                .database()
                .is_some_and(|database| self.original.database_context_is_consistent(&database.database))
            && self.statement_identity.is_valid()
            && self.original.statement_identity.as_ref() == Some(&self.statement_identity)
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(self).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityWaitFocus {
    pub participant: ActivityRequestFocus,
    pub resource: ObservedWaitResource,
}

impl ActivityWaitFocus {
    pub fn participant(&self) -> &ActivityRequestFocus {
        &self.participant
    }
    pub fn participant_mut(&mut self) -> &mut ActivityRequestFocus {
        &mut self.participant
    }
    pub fn is_valid(&self) -> bool {
        self.participant.is_valid()
            && self.participant.original.scope == ActivityScopeBasis::RequestDatabase
            && self.resource.is_valid()
            && matches!(self.resource.kind, WaitResourceKind::MemoryPage | WaitResourceKind::PageIo)
            && self
                .participant
                .original
                .wait
                .as_ref()
                .is_some_and(|wait| wait.native_type.is_some() && wait.identified_resources.contains(&self.resource))
    }
    pub fn is_valid_for(&self, subject: &CollectionSubject) -> bool {
        self.is_valid() && self.participant.is_valid_for(subject)
    }
    pub fn matches_capture_scope(&self, source: &CollectionDescription) -> bool {
        self.is_valid() && self.participant.matches_capture_scope(source)
    }
    pub fn matches_parent(
        &self,
        source: &CollectionDescription,
        original: &ActivityObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> bool {
        self.is_valid() && self.participant.matches_parent(source, original, reference)
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(self).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
}

#[cfg(test)]
mod tests;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FocusSourceState {
    Matched,
    Changed,
    Unverified,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityWaitFocusResult {
    pub focus: ActivityWaitFocus,
    pub observed_from: String,
    pub observed_until: String,
    pub source: FocusSourceState,
    pub request: PageWaitRequestState,
    pub wait: PageWaitRelation,
}

impl ActivityWaitFocusResult {
    /// A later frame keeps its own counters and SQL availability. Only exact native binding links it to the parent.
    pub fn accepts_observation(&self, page: &PageWaitObservation) -> bool {
        let Some(bounds) = ObservationBounds::new(&self.observed_from, &self.observed_until) else {
            return false;
        };
        self.is_valid()
            && page.is_valid()
            // This focus names an original coordinator observation, not any parallel task sharing its request.
            && page.task_wait.is_none()
            && self.source == FocusSourceState::Matched
            && bounds.contains(&page.observed_from)
            && bounds.contains(&page.observed_until)
            && page.request.key == self.focus.participant().original.key
            && page.request.statement_identity.as_ref() == Some(&self.focus.participant().statement_identity)
            && page.request.wait.as_ref().and_then(|wait| wait.native_type.as_ref())
                == self.focus.participant().original.wait.as_ref().and_then(|wait| wait.native_type.as_ref())
            && page.resource == self.focus.resource
    }

    pub fn is_valid(&self) -> bool {
        self.focus.is_valid()
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
            && (self.source != FocusSourceState::Matched
                || ObservationBounds::new(&self.focus.participant().original_observed_at, &self.observed_from)
                    .is_some())
            && (self.source == FocusSourceState::Matched
                || self.request == PageWaitRequestState::Unverified && self.wait == PageWaitRelation::Unqualified)
            && (self.request == PageWaitRequestState::SameStatement || self.wait == PageWaitRelation::Unqualified)
    }

    /// Eligibility for bounded header work at this observation, not proof of continuous waiting or a latch owner.
    pub fn permits_page_lookup(&self) -> bool {
        self.is_valid()
            && self.source == FocusSourceState::Matched
            && self.request == PageWaitRequestState::SameStatement
            && self.wait == PageWaitRelation::OriginalObserved
    }
}
