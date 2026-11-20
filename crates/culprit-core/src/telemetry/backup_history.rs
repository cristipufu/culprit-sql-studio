//! Retained database-native backup metadata is not collector recovery, an event feed or a restore guarantee.
use super::*;

pub const MSSQL_BACKUP_HISTORY_SOURCE: &str = "database backup history";
pub const MAX_MSSQL_BACKUP_HISTORY_ROWS: u32 = 64;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlBackupHistoryProfile {
    AzureSqlDatabase,
    SqlServerMsdb,
    ManagedInstanceMsdb,
}

/// Original read authority, independent of the historical names, initiator and recovery forks in each record.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlBackupHistoryScope {
    pub profile: MssqlBackupHistoryProfile,
    pub database_id: u32,
    pub native_database: NativeIdentity,
    /// Native deployment evidence. Unknown cannot establish an available empty Azure backup population.
    #[serde(deserialize_with = "Option::deserialize")]
    pub hyperscale: Option<bool>,
}
impl MssqlBackupHistoryScope {
    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        if !context.is_valid()
            || context.database_writable.is_none()
            || !(1..=i32::MAX as u32).contains(&self.database_id)
            || !self.native_database.is_valid()
        {
            return false;
        }
        let CollectionTarget::Database(database) = &context.target else { return false };
        if database.engine != crate::Engine::Mssql {
            return false;
        }
        let qualifiers = &self.native_database.qualifiers;
        match self.profile {
            MssqlBackupHistoryProfile::AzureSqlDatabase => {
                context.partition == SourcePartition::Target
                    && context.database_writable == Some(true)
                    && database.incarnation.as_ref().is_some_and(|id| id == &self.native_database.id)
                    && qualifiers.get("kind").is_some_and(|kind| kind == "azure-sql-logical-database")
                    && qualifiers.contains_key("physical_database_guid")
            }
            MssqlBackupHistoryProfile::SqlServerMsdb | MssqlBackupHistoryProfile::ManagedInstanceMsdb => {
                self.hyperscale.is_none()
                    && context.has_qualified_local_owner()
                    && matches!(&context.partition,SourcePartition::InstanceLocal{member:Some(member),..} if member.kind==SharedResourceKind::Instance)
                    && self.native_database.id == self.database_id.to_string()
                    && qualifiers.get("kind").is_some_and(|kind| kind == "sql-server-local-database-recovery-epoch")
                    && [
                        "database_created_without_offset",
                        "database_file_guid",
                        "database_guid",
                        "recovery_fork_guid",
                        "instance_epoch",
                    ]
                    .iter()
                    .all(|key| qualifiers.contains_key(*key))
                    && serde_json::to_vec(&self.native_database)
                        .ok()
                        .is_some_and(|raw| context.partition.native_epoch() == Some(digest(&raw).as_str()))
            }
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlBackupType {
    Database,
    DifferentialDatabase,
    Log,
    File,
    DifferentialFile,
    Partial,
    DifferentialPartial,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAzureSqlBackup {
    pub backup_file_id: String,
    pub logical_database_id: String,
    pub physical_database_name: String,
    pub logical_server_name: String,
    pub logical_database_name: String,
    /// Native datetime2(7), without a documented offset. The UTC observation point is separate.
    pub started_without_offset: String,
    pub finished_without_offset: String,
    pub backup_type: MssqlBackupType,
    pub in_retention: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlBackupLsns {
    #[serde(deserialize_with = "Option::deserialize")]
    pub first: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub checkpoint: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub database_backup: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub differential_base: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub fork_point: Option<String>,
}
impl MssqlBackupLsns {
    fn is_valid(&self) -> bool {
        [&self.first, &self.last, &self.checkpoint, &self.database_backup, &self.differential_base, &self.fork_point]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(|value| native_decimal(value, 25)))
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlBackupFlags {
    #[serde(deserialize_with = "Option::deserialize")]
    pub copy_only: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub snapshot: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub checksums: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub damaged: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub begins_log_chain: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub incomplete_metadata: Option<bool>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlBackupSoftware {
    #[serde(deserialize_with = "Option::deserialize")]
    pub vendor_id: Option<i32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub major: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub minor: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub build: Option<i16>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlMsdbBackup {
    pub backup_set_id: u32,
    pub backup_set_uuid: String,
    pub database_guid: String,
    pub database_name: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub family_guid: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub first_recovery_fork_guid: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_recovery_fork_guid: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub started_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub finished_without_offset: Option<String>,
    /// Native quarter-hour offset at backup start. NULL and127 remain distinct unknown representations.
    #[serde(deserialize_with = "Option::deserialize")]
    pub time_zone_quarter_hours: Option<i16>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub backup_type: Option<MssqlBackupType>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub size_bytes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub compressed_size_bytes: Option<String>,
    /// Historical database-derived context; none identifies a current session, application or service owner.
    #[serde(deserialize_with = "Option::deserialize")]
    pub initiator_user_name: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub server_name: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub machine_name: Option<String>,
    pub software: MssqlBackupSoftware,
    pub lsns: MssqlBackupLsns,
    pub flags: MssqlBackupFlags,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlBackupRecord {
    AzureSqlDatabase(Box<MssqlAzureSqlBackup>),
    Msdb(Box<MssqlMsdbBackup>),
}
impl MssqlBackupRecord {
    pub fn native_id(&self) -> &str {
        match self {
            Self::AzureSqlDatabase(row) => &row.backup_file_id,
            Self::Msdb(row) => &row.backup_set_uuid,
        }
    }
    pub fn backup_type(&self) -> Option<MssqlBackupType> {
        match self {
            Self::AzureSqlDatabase(row) => Some(row.backup_type),
            Self::Msdb(row) => row.backup_type,
        }
    }
    pub fn is_valid_for(&self, scope: &MssqlBackupHistoryScope, context: &NativeReadContext) -> bool {
        match (scope.profile, self) {
            (MssqlBackupHistoryProfile::AzureSqlDatabase, Self::AzureSqlDatabase(row)) => {
                scope.hyperscale == Some(false)
                    && native_identifier(&row.backup_file_id)
                    && row.logical_database_id.eq_ignore_ascii_case(&scope.native_database.id)
                    && [&row.physical_database_name, &row.logical_server_name, &row.logical_database_name]
                        .into_iter()
                        .all(|value| native_label(value))
                    && [MssqlBackupType::Database, MssqlBackupType::DifferentialDatabase, MssqlBackupType::Log]
                        .contains(&row.backup_type)
                    && native_local_clock(&row.started_without_offset, "0001")
                    && native_local_clock(&row.finished_without_offset, "0001")
            }
            (
                MssqlBackupHistoryProfile::SqlServerMsdb | MssqlBackupHistoryProfile::ManagedInstanceMsdb,
                Self::Msdb(row),
            ) => {
                (1..=i32::MAX as u32).contains(&row.backup_set_id)
                    && native_identifier(&row.backup_set_uuid)
                    && scope
                        .native_database
                        .qualifiers
                        .get("database_guid")
                        .is_some_and(|guid| row.database_guid.eq_ignore_ascii_case(guid))
                    && native_label(&row.database_name)
                    && context.target.database().is_some_and(|database| row.database_name == database.database)
                    && [&row.family_guid, &row.first_recovery_fork_guid, &row.last_recovery_fork_guid]
                        .into_iter()
                        .all(|id| id.as_deref().is_none_or(native_identifier))
                    && [&row.started_without_offset, &row.finished_without_offset]
                        .into_iter()
                        .all(|time| time.as_deref().is_none_or(|value| native_local_clock(value, "1753")))
                    && row.time_zone_quarter_hours.is_none_or(|zone| (-48..=48).contains(&zone) || zone == 127)
                    && [&row.size_bytes, &row.compressed_size_bytes]
                        .into_iter()
                        .all(|value| value.as_deref().is_none_or(|value| native_decimal(value, 20)))
                    && [&row.initiator_user_name, &row.server_name, &row.machine_name]
                        .into_iter()
                        .all(|value| value.as_deref().is_none_or(native_label))
                    && row.lsns.is_valid()
            }
            _ => false,
        }
    }
}

/// Native UUIDs and MSDB primary keys are independently unique in one returned source roster.
pub fn backup_history_roster_is_unique<'a>(records: impl IntoIterator<Item = &'a MssqlBackupRecord>) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    let mut sets = std::collections::BTreeSet::new();
    records.into_iter().all(|record| {
        ids.insert(record.native_id().to_ascii_lowercase())
            && match record {
                MssqlBackupRecord::AzureSqlDatabase(_) => true,
                MssqlBackupRecord::Msdb(row) => sets.insert(row.backup_set_id),
            }
    })
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlBackupHistoryObservation {
    pub context: NativeReadContext,
    pub scope: MssqlBackupHistoryScope,
    pub observed_at: String,
    pub position: u32,
    pub record: MssqlBackupRecord,
}
impl MssqlBackupHistoryObservation {
    pub fn is_valid(&self) -> bool {
        self.scope.matches_context(&self.context)
            && (1..=MAX_MSSQL_BACKUP_HISTORY_ROWS).contains(&self.position)
            && ObservationBounds::new(&self.context.observed_from, &self.context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
            && self.record.is_valid_for(&self.scope, &self.context)
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(&self.scope, self.record.native_id())).map(|raw| digest(&raw)).map_err(|_| invalid())
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let SourceObservation::BackupHistory { scope, .. } = &source.observation else {
            return false;
        };
        self.is_valid()
            && self.scope == **scope
            && source.observation.snapshot().is_some_and(|snapshot| self.position <= snapshot.records)
            && backup_history_context(source).is_ok_and(|context| context == self.context)
    }
}

pub fn backup_history_header_is_valid(snapshot: &StateSnapshotDescription) -> bool {
    snapshot.is_valid()
        && snapshot.dataset == SnapshotDataset::BackupHistory
        && snapshot.records <= MAX_MSSQL_BACKUP_HISTORY_ROWS
        && matches!(snapshot.sources.as_slice(),[source] if source.source==MSSQL_BACKUP_HISTORY_SOURCE
            && source.records==snapshot.records && !source.complete_scope
            && matches!(source.state, crate::SourceState::Partial | crate::SourceState::Unavailable))
}
/// The same nonrecursive original-header guard serves raw source-fit and canonical consumers, including empty reads.
pub fn backup_history_context(source: &CollectionDescription) -> Result<NativeReadContext> {
    let SourceObservation::BackupHistory { snapshot, scope, database_writable } = &source.observation else {
        return Err(invalid());
    };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: snapshot.observed_from.clone(),
        observed_until: snapshot.observed_until.clone(),
        database_writable: Some(*database_writable),
    };
    if source.source != MSSQL_BACKUP_HISTORY_SOURCE
        || source.continuity != SourceContinuity::Unverified
        || !backup_history_header_is_valid(snapshot)
        || !scope.matches_context(&context)
    {
        return Err(invalid());
    }
    let [coverage] = snapshot.sources.as_slice() else {
        return Err(invalid());
    };
    if scope.profile == MssqlBackupHistoryProfile::AzureSqlDatabase
        && scope.hyperscale != Some(false)
        && (coverage.state != crate::SourceState::Unavailable || coverage.records != 0)
    {
        return Err(invalid());
    }
    Ok(context)
}
fn native_label(value: &str) -> bool {
    value.encode_utf16().count() <= 128
}
fn native_local_clock(value: &str, minimum_year: &str) -> bool {
    value.len() == 27 && value.is_ascii() && &value[..4] >= minimum_year && super::catalog::local_time(value)
}
fn native_identifier(value: &str) -> bool {
    valid_key(value) && value.len() <= 128
}
fn native_decimal(value: &str, max_digits: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_digits
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
pub(crate) mod tests;
