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
    MissingRequest,
    ChangedRequest,
    ChangedMetadata,
    UnverifiedSource,
    UnrecognizedAddress,
    Limit,
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
    /// Source-scoped native statement/plan handles and offsets agreed at both request checks; not global IDs.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub statement_identity: Option<NativeIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan_identity: Option<NativeIdentity>,
    pub resource: ObservedWaitResource,
    pub resolution: PageWaitResolution,
    pub target: Option<Box<MappedPageTarget>>,
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
        valid_key(&self.source)
            && self.request.is_valid()
            && self.request.key.request.is_some()
            && self.statement_identity.as_ref().is_none_or(NativeIdentity::is_valid)
            && self.plan_identity.as_ref().is_none_or(NativeIdentity::is_valid)
            && (self.plan_identity.is_none() || self.statement_identity.is_some())
            && (!matches!(
                self.resolution,
                PageWaitResolution::MissingRequest
                    | PageWaitResolution::ChangedRequest
                    | PageWaitResolution::UnverifiedSource
            ) || (self.statement_identity.is_none() && self.plan_identity.is_none()))
            && self.resource.is_valid()
            && matches!(self.resource.kind, WaitResourceKind::MemoryPage | WaitResourceKind::PageIo)
            && self.request.wait.as_ref().is_some_and(|wait| wait.identified_resources.contains(&self.resource))
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
            && (self.resolution == PageWaitResolution::ObservedAtBothBounds) == self.target.is_some()
            && self
                .target
                .as_ref()
                .is_none_or(|target| target.is_valid() && self.request.scope == ActivityScopeBasis::RequestDatabase)
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
