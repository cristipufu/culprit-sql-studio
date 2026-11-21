//! Exact storage points keep allocation populations separate from a visible database-file census.
use super::*;
use crate::{FileKind, FileScope};

const MAX_STORAGE_SOURCES: usize = 3;
const MAX_STORAGE_RETAINED_ROWS: usize = 30_000;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum StoragePopulation {
    DatabaseTransactionLog,
    WritableOnlineDataFilegroups,
    SharedTempdbDataFiles,
    SharedTempdbTransactionLog,
}
impl StoragePopulation {
    pub fn matches(self, kind: FileKind, scope: FileScope) -> bool {
        matches!(
            (self, kind, scope),
            (Self::DatabaseTransactionLog, FileKind::Log, FileScope::Database)
                | (Self::WritableOnlineDataFilegroups, FileKind::Data, FileScope::Database)
                | (Self::SharedTempdbDataFiles, FileKind::Temp, FileScope::Server)
                | (Self::SharedTempdbTransactionLog, FileKind::Log, FileScope::Server)
        )
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StoragePointRef {
    pub source: String,
    pub key: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum StorageFileSizeBasis {
    Allocated,
    ContainerUsed,
    SnapshotMaximum,
    Unknown,
}

/// Native sys.database_files shape. It is not a portable filesystem or physical execution-owner identity.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlDatabaseFile {
    pub native_object: String,
    pub kind: FileKind,
    pub size_basis: StorageFileSizeBasis,
    pub page_size_bytes: u32,
    pub observed_at: String,
    pub database: String,
    pub native_database_id: String,
    pub file_id: String,
    pub file_guid: Option<String>,
    pub native_type: String,
    pub type_description: Option<String>,
    pub native_state: String,
    pub state_description: Option<String>,
    pub data_space_id: String,
    pub logical_name: String,
    /// Native size in 8-KiB pages; FILESTREAM reports used container size, and snapshots report maximum size.
    pub size_pages: String,
    /// Native -1 is unbounded-by-this-setting, not measured filesystem space. Zero disables growth.
    pub maximum_size_pages: String,
    pub growth: String,
    pub growth_is_percent: Option<bool>,
    pub is_read_only: Option<bool>,
    pub filegroup_read_only: Option<bool>,
    pub is_sparse: Option<bool>,
    pub database_is_snapshot: Option<bool>,
    /// FILEPROPERTY SpaceUsed only for a readable online ROWS file; other file types stay unknown.
    pub used_pages: Option<String>,
}

/// One native catalog policy shared by acquisition and admission of saved evidence. Sparse-file flags stay
/// independent: they do not establish database snapshot identity, and offline snapshot files may not be sparse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MssqlDatabaseFileProfile {
    pub kind: FileKind,
    pub size_basis: StorageFileSizeBasis,
    pub used_pages_supported: bool,
}
impl MssqlDatabaseFileProfile {
    pub fn from_native(native_type: u8, native_state: u8, database_is_snapshot: Option<bool>) -> Self {
        Self {
            kind: match native_type {
                0 => FileKind::Data,
                1 => FileKind::Log,
                _ => FileKind::Other,
            },
            size_basis: match (database_is_snapshot, native_type) {
                (Some(true), _) => StorageFileSizeBasis::SnapshotMaximum,
                (Some(false), 0 | 1) => StorageFileSizeBasis::Allocated,
                (Some(false), 2) => StorageFileSizeBasis::ContainerUsed,
                _ => StorageFileSizeBasis::Unknown,
            },
            used_pages_supported: native_type == 0 && native_state == 0,
        }
    }
}
impl MssqlDatabaseFile {
    pub const PAGE_SIZE_BYTES: u32 = 8192;

    pub fn is_valid(&self) -> bool {
        let (Some(native_type), Some(native_state)) =
            (storage_native_code(&self.native_type), storage_native_code(&self.native_state))
        else {
            return false;
        };
        let profile = MssqlDatabaseFileProfile::from_native(native_type, native_state, self.database_is_snapshot);
        self.native_object == "sys.database_files"
            && self.page_size_bytes == Self::PAGE_SIZE_BYTES
            && self.kind == profile.kind
            && self.size_basis == profile.size_basis
            && (self.used_pages.is_none() || profile.used_pages_supported)
            && storage_time(&self.observed_at)
            && storage_name(&self.database)
            && storage_name(&self.logical_name)
            && storage_positive(&self.native_database_id)
            && storage_positive(&self.file_id)
            && self.file_guid.as_deref().is_none_or(storage_name)
            && [&self.type_description, &self.state_description]
                .into_iter()
                .all(|v| v.as_deref().is_none_or(storage_name))
            && [&self.data_space_id, &self.size_pages, &self.growth].into_iter().all(|v| valid_counter(v))
            && (self.maximum_size_pages == "-1" || valid_counter(&self.maximum_size_pages))
            && self.used_pages.as_deref().is_none_or(valid_counter)
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(storage_invalid());
        }
        Ok(format!("file:{}", self.file_id))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StorageRead {
    pub target: CollectionTarget,
    pub context: ObservationReadContext,
    pub sources: Vec<StateSourceCoverage>,
    pub observations: Vec<OperationalObservation>,
    pub read_complete: bool,
}
impl StorageRead {
    pub fn from_source(
        source: &CollectionDescription,
        observations: Vec<OperationalObservation>,
        read_complete: bool,
    ) -> Result<Self> {
        let header = source.observation.snapshot().ok_or_else(storage_invalid)?;
        if header.dataset != SnapshotDataset::Operational {
            return Err(storage_invalid());
        }
        let sources = header.sources.iter().filter(|source| Self::recognizes(&source.source)).cloned().collect();
        let observations = observations.into_iter().filter(|row| Self::recognizes(&row.source)).collect();
        let read = Self {
            target: source.target.clone(),
            context: ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(source)?) },
            sources,
            observations,
            read_complete,
        };
        read.is_valid().then_some(read).ok_or_else(storage_invalid)
    }
    pub fn recognizes(source: &str) -> bool {
        matches!(
            source,
            crate::source_id::LOG_CAPACITY | crate::source_id::FILE_CAPACITY | crate::source_id::TEMP_CAPACITY
        )
    }
    pub fn is_valid(&self) -> bool {
        if !self.target.is_valid()
            || !self.context.is_valid()
            || self.sources.is_empty()
            || self.sources.len() > MAX_STORAGE_SOURCES
            || self.observations.len() > MAX_STORAGE_RETAINED_ROWS
            || self.context.native().is_some_and(|context| context.target != self.target)
        {
            return false;
        }
        let mut sources = std::collections::BTreeSet::new();
        if self.sources.iter().any(|source| {
            !Self::recognizes(&source.source) || !source.is_valid() || !sources.insert(source.source.as_str())
        }) {
            return false;
        }
        let mut keys = std::collections::BTreeSet::new();
        for row in &self.observations {
            if !row.is_valid() || !sources.contains(row.source.as_str()) || !keys.insert((&row.source, &row.key)) {
                return false;
            }
            let (at, database) = match &row.fact {
                OperationalFact::Space(space) => {
                    if space.population == StoragePopulation::SharedTempdbTransactionLog
                        && (row.source != crate::source_id::LOG_CAPACITY
                            || !space.has_shared_tempdb_log_owner(self.context.native()))
                    {
                        return false;
                    }
                    (&space.observed_at, space.database.as_deref())
                }
                OperationalFact::MssqlDatabaseFile(file) => {
                    if !file.fits_target(&self.target) {
                        return false;
                    }
                    (&file.observed_at, Some(file.database.as_str()))
                }
                _ => return false,
            };
            if self.context.native().is_some_and(|context| {
                !ObservationBounds::new(&context.observed_from, &context.observed_until)
                    .is_some_and(|bounds| bounds.contains(at))
            }) {
                return false;
            }
            if database.is_some_and(|database| self.target.database().is_none_or(|target| target.database != database))
            {
                return false;
            }
        }
        self.sources.iter().all(|source| {
            let count = self.observations.iter().filter(|row| row.source == source.source).count() as u32;
            count <= source.records
                && (!self.read_complete || count == source.records)
                && (count == 0 || matches!(source.state, crate::SourceState::Collected | crate::SourceState::Partial))
        })
    }
    pub fn space(&self, reference: &StoragePointRef) -> Option<&OperationalSpace> {
        if !self.is_valid() {
            return None;
        }
        let row = self.observations.iter().find(|row| row.source == reference.source && row.key == reference.key)?;
        let OperationalFact::Space(space) = &row.fact else { return None };
        Some(space)
    }
}
pub(super) fn storage_time(at: &str) -> bool {
    at.len() <= 64 && ObservationBounds::new(at, at).is_some()
}
pub(super) fn storage_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 512
}
fn storage_positive(value: &str) -> bool {
    value.parse::<u32>().is_ok_and(|v| v > 0 && v.to_string() == value)
}
fn storage_native_code(value: &str) -> Option<u8> {
    value.parse::<u8>().ok().filter(|code| code.to_string() == value)
}
fn storage_invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

impl OperationalSpace {
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        if self.population == StoragePopulation::SharedTempdbTransactionLog
            && !NativeReadContext::from_snapshot(source)
                .is_ok_and(|context| self.has_shared_tempdb_log_owner(Some(&context)))
        {
            return false;
        }
        storage_scope_fits_source(source, &self.observed_at, self.database.as_deref())
    }

    fn has_shared_tempdb_log_owner(&self, context: Option<&NativeReadContext>) -> bool {
        self.database.is_none()
            && self.parts.is_empty()
            && context.is_some_and(|context| {
                context.has_qualified_local_owner()
                    && matches!((&context.target, &context.partition),
                        (CollectionTarget::Shared(owner), SourcePartition::InstanceLocal { member: Some(member), .. })
                        if owner.engine == crate::Engine::Mssql
                            && owner.kind == SharedResourceKind::Instance
                            && owner == member)
            })
    }
}
impl MssqlDatabaseFile {
    fn fits_target(&self, target: &CollectionTarget) -> bool {
        target.engine() == crate::Engine::Mssql
            && target.database().is_some_and(|database| database.database == self.database)
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        self.fits_target(&source.target) && storage_scope_fits_source(source, &self.observed_at, Some(&self.database))
    }
}
fn storage_scope_fits_source(source: &CollectionDescription, at: &str, database: Option<&str>) -> bool {
    if database.is_some_and(|database| source.target.database().is_none_or(|target| target.database != database)) {
        return false;
    }
    match &source.observation {
        SourceObservation::PagedSnapshot(header) | SourceObservation::PagedSourceRead { snapshot: header, .. } => {
            // Acquisition has only a declared lower bound. The final closed description must admit every row
            // again before publication; accepting this point does not establish completion or an end clock.
            header.dataset == SnapshotDataset::Operational
                && header.is_valid()
                && source.partition_is_consistent()
                && ObservationBounds::new(&header.observed_from, at).is_some()
        }
        _ => {
            source.observation.snapshot().is_some_and(|header| header.dataset == SnapshotDataset::Operational)
                && NativeReadContext::from_snapshot(source).is_ok_and(|context| {
                    ObservationBounds::new(&context.observed_from, &context.observed_until)
                        .is_some_and(|bounds| bounds.contains(at))
                })
        }
    }
}

#[cfg(test)]
mod tests;
