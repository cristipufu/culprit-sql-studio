//! Mapping an observed wait target identifies current metadata and affected participants. It does not establish
//! continuous page identity, an allocation's origin or the request that owns a transient latch.
use super::*;
use crate::{ObservedWaitResource, WaitResourceKind};
use std::collections::BTreeSet;

pub const MAX_PAGE_WAIT_OBSERVATIONS: usize = 64;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageWaitResolution {
    ObservedAtBothBounds,
    OutsideDatabase,
    Unsupported,
    PermissionDenied,
    Unavailable,
    ChangedMetadata,
    UnverifiedSource,
    UnrecognizedAddress,
    Limit,
}

/// Whether the later native request still matches the originally observed generation and statement identity.
/// This is independent of whether its short-lived page wait remains active.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageWaitRequestState {
    SameStatement,
    Missing,
    Changed,
    Unverified,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageWaitRelation {
    OriginalObserved,
    NoLongerObserved,
    Different,
    Unqualified,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageWaitRecheck {
    pub observed_at: String,
    pub request: PageWaitRequestState,
    pub wait: PageWaitRelation,
}

impl PageWaitRecheck {
    pub fn is_valid(&self) -> bool {
        canonical_time(&self.observed_at).is_ok()
            && (self.request == PageWaitRequestState::SameStatement || self.wait == PageWaitRelation::Unqualified)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageTargetIdentityBasis {
    DatabaseIncarnation,
    LocalDatabaseFileEpoch,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageWaitTargetKind {
    IndexLeaf,
    IndexBranch,
    HeapData,
    Allocation,
    Unclassified,
}

/// Source-decoded coordinates of the originally observed wait. Native syntax and incarnation identities remain
/// provider-owned; these coordinates bind later header evidence to the requested page without reparsing raw text.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageWaitAddress {
    pub database_id: u32,
    pub file_id: u32,
    pub page_id: u32,
}

impl PageWaitAddress {
    pub fn is_valid(&self) -> bool {
        self.database_id > 0 && self.file_id > 0
    }

    fn matches_header(&self, header: &PageHeaderFacts) -> bool {
        self.database_id == header.database_id && self.file_id == header.file_id && self.page_id == header.page_id
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageHeaderFacts {
    pub database_id: u32,
    pub file_id: u32,
    pub page_id: u32,
    pub header_version: Option<u32>,
    pub page_type: Option<u32>,
    pub level: Option<u32>,
    /// Native identifiers preserve signed representations; they are not nonnegative counters.
    pub object_id: Option<String>,
    pub index_id: Option<String>,
    pub partition_id: Option<String>,
    pub allocation_unit_id: Option<String>,
    pub lsn: Option<String>,
    pub next_file_id: Option<u32>,
    pub next_page_id: Option<u32>,
}

impl PageHeaderFacts {
    fn is_valid(&self) -> bool {
        self.database_id > 0
            && self.file_id > 0
            && [&self.object_id, &self.index_id, &self.partition_id, &self.allocation_unit_id]
                .into_iter()
                .flatten()
                .all(|value| value.len() <= 21 && value.parse::<i64>().is_ok())
            && self.lsn.as_deref().is_none_or(identifier)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageWaitObject {
    pub native: NativeIdentity,
    pub schema: String,
    pub name: String,
    pub index_name: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MappedPageTarget {
    pub identity_basis: PageTargetIdentityBasis,
    pub database: NativeIdentity,
    pub file: NativeIdentity,
    pub header: PageHeaderFacts,
    pub kind: PageWaitTargetKind,
    /// Missing catalog visibility preserves the header without inventing an object or index label.
    pub object: Option<PageWaitObject>,
}

/// Index-wide point counters are independent of the request's short-lived wait and the page's later mapping.
/// Equal endpoint catalog observations do not establish a continuous counter epoch or an individual latch owner.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageIndexOperationRelation {
    CheckedTarget,
    ChangedTarget,
    UnverifiedTarget,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case")]
#[ts(export)]
pub enum PageIndexOperations {
    Observed {
        observed_at: String,
        counters: Box<MssqlIndexOperations>,
        partition_id: Option<String>,
        object: Option<NativeIdentity>,
        relation: PageIndexOperationRelation,
    },
    Missing,
    PermissionDenied,
    Unavailable,
}

impl PageIndexOperations {
    /// Structural equality is necessary for a link, but the provider must also qualify source/catalog bounds.
    pub fn matches_target(&self, target: &MappedPageTarget) -> bool {
        let Self::Observed { counters, partition_id, object, .. } = self else { return false };
        let identity = &counters.identity;
        target.header.database_id == identity.database_id
            && target.header.object_id.as_deref().and_then(|id| id.parse::<u32>().ok()) == Some(identity.object_id)
            && target.header.index_id.as_deref().and_then(|id| id.parse::<u32>().ok()) == Some(identity.index_id)
            && partition_id.is_some()
            && target.header.partition_id == *partition_id
            && object.is_some()
            && target.object.as_ref().map(|target| &target.native) == object.as_ref()
    }

    fn fits_observation(&self, observation: &PageWaitObservation, bounds: &ObservationBounds) -> bool {
        let Some(address) = observation.observed_address else { return false };
        if observation.request.scope != ActivityScopeBasis::RequestDatabase {
            return false;
        }
        let Self::Observed { observed_at, counters, partition_id, object, relation } = self else { return true };
        let identity = &counters.identity;
        counters.is_valid()
            && identity.database_id == address.database_id
            && identity.database_id <= i16::MAX as u32
            && identity.object_id <= i32::MAX as u32
            && identity.index_id <= i32::MAX as u32
            && identity.partition_number <= i32::MAX as u32
            && bounds.contains(observed_at)
            && partition_id.as_deref().is_none_or(|id| id.len() <= 21 && id.parse::<i64>().is_ok_and(|id| id > 0))
            && object.as_ref().is_none_or(|object| object.is_valid() && object.id == identity.object_id.to_string())
            && (*relation != PageIndexOperationRelation::CheckedTarget
                || observation.target.as_ref().is_some_and(|target| self.matches_target(target)))
    }
}

impl MappedPageTarget {
    pub fn is_valid(&self) -> bool {
        self.database.is_valid()
            && self.file.is_valid()
            && self.header.is_valid()
            && self.object.as_ref().is_none_or(|object| {
                object.native.is_valid()
                    && identifier(&object.schema)
                    && identifier(&object.name)
                    && object.index_name.as_deref().is_none_or(identifier)
            })
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PageWaitObservation {
    pub source: String,
    pub observed_from: String,
    pub observed_until: String,
    /// The originally observed waiter. It remains historical context if later request checks fail.
    pub request: ActivityObservation,
    /// Later request and wait-state checks do not rewrite the originally observed participant or page address.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub before_check: Option<PageWaitRecheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub after_check: Option<PageWaitRecheck>,
    /// Source-scoped native statement/plan handles and offsets agreed at both request checks; not global IDs.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub statement_identity: Option<NativeIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan_identity: Option<NativeIdentity>,
    pub resource: ObservedWaitResource,
    /// None means the original raw wait could not be decoded into qualified page coordinates. It remains visible
    /// as unresolved evidence and cannot acquire a mapped header.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub observed_address: Option<PageWaitAddress>,
    pub resolution: PageWaitResolution,
    pub target: Option<Box<MappedPageTarget>>,
    /// Independently observed index counters remain available when a later page/catalog check cannot link them.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub index_operations: Option<PageIndexOperations>,
}

impl PageWaitObservation {
    /// An unresolved request from another scope remains original context, never a mapped database target.
    pub fn fits_database_capture(&self, database: &str) -> bool {
        self.request.database_context_is_consistent(database)
    }

    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&(&self.request.key, &self.resource))
            .map(|value| digest(&value))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }

    pub fn is_valid(&self) -> bool {
        let Some(bounds) = ObservationBounds::new(&self.observed_from, &self.observed_until) else { return false };
        let checks = [&self.before_check, &self.after_check];
        let statement_checked = checks
            .into_iter()
            .all(|check| check.as_ref().is_some_and(|check| check.request == PageWaitRequestState::SameStatement));
        valid_key(&self.source)
            && self.request.is_valid()
            && self.request.key.request.is_some()
            && self.statement_identity.as_ref().is_none_or(NativeIdentity::is_valid)
            && self.plan_identity.as_ref().is_none_or(NativeIdentity::is_valid)
            && (self.plan_identity.is_none() || self.statement_identity.is_some())
            && (self.statement_identity.is_none() || statement_checked)
            && (self.resolution != PageWaitResolution::UnverifiedSource || self.statement_identity.is_none())
            && checks.into_iter().flatten().all(|check| check.is_valid() && bounds.contains(&check.observed_at))
            && match (&self.before_check, &self.after_check) {
                (Some(before), Some(after)) => {
                    ObservationBounds::new(&before.observed_at, &after.observed_at).is_some()
                }
                _ => true,
            }
            && self.resource.is_valid()
            && self.observed_address.as_ref().is_none_or(PageWaitAddress::is_valid)
            && matches!(self.resource.kind, WaitResourceKind::MemoryPage | WaitResourceKind::PageIo)
            && self.request.wait.as_ref().is_some_and(|wait| wait.identified_resources.contains(&self.resource))
            && (self.resolution == PageWaitResolution::ObservedAtBothBounds) == self.target.is_some()
            && self.target.as_ref().is_none_or(|target| {
                target.is_valid()
                    && self.request.scope == ActivityScopeBasis::RequestDatabase
                    && self.observed_address.as_ref().is_some_and(|address| address.matches_header(&target.header))
            })
            && self.index_operations.as_ref().is_none_or(|point| point.fits_observation(self, &bounds))
    }

    pub fn redact(&mut self, engine: crate::Engine) {
        self.request.redact(engine);
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PageWaitSnapshot {
    pub source: String,
    pub observed_from: String,
    pub observed_until: String,
    pub observations: Vec<PageWaitObservation>,
}

impl PageWaitSnapshot {
    pub fn is_valid(&self) -> bool {
        let Some(bounds) = ObservationBounds::new(&self.observed_from, &self.observed_until) else {
            return false;
        };
        let mut keys = BTreeSet::new();
        valid_key(&self.source)
            && self.observations.len() <= MAX_PAGE_WAIT_OBSERVATIONS
            && self.observations.iter().all(|row| {
                row.is_valid()
                    && row.source == self.source
                    && bounds.contains(&row.observed_from)
                    && bounds.contains(&row.observed_until)
                    && row.key().is_ok_and(|key| keys.insert(key))
            })
    }
}

#[cfg(test)]
mod tests;
