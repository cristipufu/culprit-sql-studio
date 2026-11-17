//! Mapping an observed wait target identifies current metadata and affected participants. It does not establish
//! continuous page identity, an allocation's origin or the request that owns a transient latch.
use super::*;
use crate::{ObservedWaitResource, WaitResourceKind};
mod resource;
use std::collections::BTreeSet;

mod tasks;
pub use tasks::MssqlPageTaskWait;
mod holders;
pub use holders::*;
mod writers;
pub use writers::*;

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
    Pfs,
    Gam,
    Sgam,
    Iam,
    Unclassified,
}

/// SQL Server's native numeric header layout, not a cross-provider page-type convention or proof of an owner.
/// A provider must also observe rowstore catalog type 1/2 before publishing an index leaf/branch classification.
/// LIMITED can omit the version while retaining numeric fields that qualify a known layout; no version is inferred.
/// A present unfamiliar version cannot use the known layout.
/// Native types 8/9/10/11 are demonstrated by the storage-engine author:
/// https://www.sqlskills.com/blogs/paul/inside-the-storage-engine-using-dbcc-page-and-dbcc-ind-to-find-out-if-page-splits-ever-roll-back/
/// Header fields and LIMITED semantics:
/// https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-page-info-transact-sql
pub fn classify_sql_server_page_header(header: &PageHeaderFacts) -> PageWaitTargetKind {
    if header.header_version.is_some_and(|version| version != 1) {
        return PageWaitTargetKind::Unclassified;
    }
    match (header.page_type, header.level, header.index_id.as_deref()) {
        (Some(8), _, _) => PageWaitTargetKind::Gam,
        (Some(9), _, _) => PageWaitTargetKind::Sgam,
        (Some(10), _, _) => PageWaitTargetKind::Iam,
        (Some(11), _, _) => PageWaitTargetKind::Pfs,
        (Some(1), Some(0), Some("0")) => PageWaitTargetKind::HeapData,
        (Some(1 | 2), Some(0), Some(index)) if index.parse::<i32>().is_ok_and(|id| id > 0) => {
            PageWaitTargetKind::IndexLeaf
        }
        (Some(2), Some(level), Some(index)) if level > 0 && index.parse::<i32>().is_ok_and(|id| id > 0) => {
            PageWaitTargetKind::IndexBranch
        }
        _ => PageWaitTargetKind::Unclassified,
    }
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
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PageTargetScope {
    RequestDatabase,
    /// The native resource reader's authority is independent of the affected request's database.
    SharedResource {
        context: Box<NativeReadContext>,
    },
}

impl PageTargetScope {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::RequestDatabase => true,
            Self::SharedResource { context } => {
                context.has_qualified_local_owner()
                    && matches!((&context.target, &context.partition), (
                        CollectionTarget::Shared(resource),
                        SourcePartition::InstanceLocal { member: Some(member), .. }
                    ) if resource == member)
            }
        }
    }

    /// Content binding only. This does not manufacture native authority or a new physical generation: the
    /// provider must independently verify the resource, native identities and source bounds.
    pub fn bind_identities(
        &self,
        database: NativeIdentity,
        file: NativeIdentity,
    ) -> Result<(NativeIdentity, NativeIdentity)> {
        let Some(scope) = self.binding_key()? else { return Ok((database, file)) };
        let database = database.qualified("page_target_scope", &scope);
        let database_key = digest(&serde_json::to_vec(&database).map_err(|_| CollectionStop::InvalidPage.error())?);
        let file = file.qualified("page_target_scope", scope).qualified("page_target_database", database_key);
        Ok((database, file))
    }

    fn binding_key(&self) -> Result<Option<String>> {
        match self {
            Self::RequestDatabase => Ok(None),
            Self::SharedResource { context } => {
                if !self.is_valid() {
                    return Err(CollectionStop::InvalidPage.error());
                }
                serde_json::to_vec(&(&context.target, &context.partition))
                    .map(|bytes| Some(digest(&bytes)))
                    .map_err(|_| CollectionStop::InvalidPage.error())
            }
        }
    }

    fn matches_identities(&self, database: &NativeIdentity, file: &NativeIdentity) -> bool {
        let Ok(Some(scope)) = self.binding_key() else {
            return matches!(self, Self::RequestDatabase)
                && !database.qualifiers.contains_key("page_target_scope")
                && !file.qualifiers.contains_key("page_target_scope")
                && !file.qualifiers.contains_key("page_target_database");
        };
        let Ok(bytes) = serde_json::to_vec(database) else { return false };
        database.qualifiers.get("page_target_scope") == Some(&scope)
            && file.qualifiers.get("page_target_scope") == Some(&scope)
            && file.qualifiers.get("page_target_database") == Some(&digest(&bytes))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MappedPageTarget {
    pub scope: PageTargetScope,
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
        self.scope.is_valid()
            && self.database.is_valid()
            && self.file.is_valid()
            && self.scope.matches_identities(&self.database, &self.file)
            && self.header.is_valid()
            && (self.kind == PageWaitTargetKind::Unclassified
                || self.kind == classify_sql_server_page_header(&self.header))
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
    /// A native task wait is distinct from the coordinator's request-level wait. Addresses identify only the
    /// observed task slot; they do not establish a persistent task generation or an owner session incarnation.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_wait: Option<MssqlPageTaskWait>,
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

/// Borrowed native inputs for explanation and validation, without replacing the original coordinator context.
pub struct PageWaitObservedWait<'a> {
    pub native_type: Option<&'a str>,
    pub elapsed_ms: Option<&'a str>,
    pub resource: &'a ObservedWaitResource,
}

impl PageWaitObservation {
    /// Shared target authority must come from this exact captured source. The affected request's database is
    /// independent; an instance target does not move that request into the temporary database.
    pub fn fits_source_scope(&self, source: &CollectionDescription) -> bool {
        let Some(snapshot) = source.observation.snapshot() else { return false };
        let Some(bounds) = ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until) else {
            return false;
        };
        if !self.is_valid()
            || !source.partition_is_consistent()
            || snapshot.dataset != SnapshotDataset::PageWaitTargets
            || !bounds.contains(&self.observed_from)
            || !bounds.contains(&self.observed_until)
        {
            return false;
        }
        match self.target.as_ref().map(|target| &target.scope) {
            Some(PageTargetScope::SharedResource { context }) => {
                context.target == source.target
                    && context.partition == source.partition
                    && bounds.contains(&context.observed_from)
                    && bounds.contains(&context.observed_until)
            }
            Some(PageTargetScope::RequestDatabase) => {
                source.target.database().is_some_and(|database| self.fits_database_capture(&database.database))
            }
            None => match &source.target {
                CollectionTarget::Database(database) => self.fits_database_capture(&database.database),
                CollectionTarget::Shared(_) => source.is_partition_qualified(),
            },
        }
    }

    pub fn observed_wait(&self) -> PageWaitObservedWait<'_> {
        match &self.task_wait {
            Some(task) => PageWaitObservedWait {
                native_type: Some(&task.wait_type),
                elapsed_ms: task.wait_duration_ms.as_deref(),
                resource: &task.resource,
            },
            None => PageWaitObservedWait {
                native_type: self.request.wait.as_ref().and_then(|wait| wait.native_type.as_deref()),
                elapsed_ms: self.request.wait.as_ref().and_then(|wait| wait.elapsed_ms.as_deref()),
                resource: &self.resource,
            },
        }
    }

    /// An unresolved request from another scope remains original context, never a mapped database target.
    pub fn fits_database_capture(&self, database: &str) -> bool {
        self.request.database_context_is_consistent(database)
    }

    /// A database-owned follow-up cannot borrow mapping authority from a separately scoped shared resource.
    pub fn fits_database_lookup(&self, database: &str) -> bool {
        self.fits_database_capture(database)
            && self.target.as_ref().is_none_or(|target| matches!(target.scope, PageTargetScope::RequestDatabase))
    }

    pub fn key(&self) -> Result<String> {
        let bytes = match &self.task_wait {
            Some(task) => {
                serde_json::to_vec(&(&self.request.key, &self.resource, &task.task_address, task.execution_context_id))
            }
            None => serde_json::to_vec(&(&self.request.key, &self.resource)),
        };
        bytes.map(|value| digest(&value)).map_err(|_| CollectionStop::InvalidPage.error())
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
            && (self.task_wait.is_none()
                || self.statement_identity.is_none()
                || checks
                    .into_iter()
                    .all(|check| check.as_ref().is_some_and(|check| check.wait == PageWaitRelation::OriginalObserved)))
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
            && match &self.task_wait {
                Some(task) => {
                    task.is_valid()
                        && task.matches_request(&self.request)
                        && task.observed_at == self.observed_from
                        && task.resource == self.resource
                }
                None => {
                    self.request.wait.as_ref().is_some_and(|wait| wait.identified_resources.contains(&self.resource))
                }
            }
            && (self.resolution == PageWaitResolution::ObservedAtBothBounds) == self.target.is_some()
            && self.target.as_ref().is_none_or(|target| {
                target.is_valid()
                    && self.request.scope == ActivityScopeBasis::RequestDatabase
                    && self.observed_address.as_ref().is_some_and(|address| address.matches_header(&target.header))
                    && match &target.scope {
                        PageTargetScope::RequestDatabase => true,
                        PageTargetScope::SharedResource { context } => {
                            let Some(shared) = ObservationBounds::new(&context.observed_from, &context.observed_until)
                            else {
                                return false;
                            };
                            shared.contains(&self.observed_from)
                                && bounds.contains(&context.observed_until)
                                && checks.into_iter().flatten().all(|check| shared.contains(&check.observed_at))
                        }
                    }
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
                    && row.target.as_ref().is_none_or(|target| match &target.scope {
                        PageTargetScope::RequestDatabase => true,
                        PageTargetScope::SharedResource { context } => {
                            bounds.contains(&context.observed_from) && bounds.contains(&context.observed_until)
                        }
                    })
                    && row.key().is_ok_and(|key| keys.insert(key))
            })
    }
}

#[cfg(test)]
mod tests;
