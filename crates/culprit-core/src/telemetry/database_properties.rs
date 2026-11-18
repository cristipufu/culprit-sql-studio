//! Selected-database catalog facts retain their native clock and scope independently of current scan ownership.
use super::*;

pub const DATABASE_PROPERTIES_SOURCE: &str = "database-properties";
const MAX_NATIVE_TEXT_BYTES: usize = 512;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DatabasePropertiesProfile {
    MssqlSysDatabases,
}

/// A source code is independent from its native display description. Descriptions drive no interpretation.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DatabasePropertyCode {
    pub code: Option<u8>,
    pub description: Option<String>,
}
impl DatabasePropertyCode {
    fn is_valid(&self) -> bool {
        self.description.as_deref().is_none_or(valid_native_text)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DatabasePropertyCodeKind {
    CatalogState,
    UserAccess,
    SnapshotIsolation,
    PageVerify,
    RecoveryModel,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DatabasePropertyState {
    Online,
    Restoring,
    Recovering,
    RecoveryPending,
    Suspect,
    Emergency,
    Offline,
    Copying,
    OfflineSecondary,
    MultiUser,
    SingleUser,
    RestrictedUser,
    Off,
    On,
    TransitionToOff,
    TransitionToOn,
    NoVerification,
    TornPageDetection,
    Checksum,
    FullRecovery,
    BulkLoggedRecovery,
    SimpleRecovery,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DatabasePropertiesObservation {
    pub profile: DatabasePropertiesProfile,
    pub database: String,
    /// Native database_id is local to this observation; it is not an incarnation or a replica identity.
    pub native_database_id: String,
    pub observed_at: String,
    pub catalog_state: DatabasePropertyCode,
    pub user_access: DatabasePropertyCode,
    /// Native DATABASEPROPERTYEX Updateability, from this same selected-database read.
    pub updateability: Option<String>,
    pub catalog_read_only: Option<bool>,
    pub collation: Option<String>,
    pub compatibility_level: Option<u8>,
    pub snapshot_isolation: DatabasePropertyCode,
    pub read_committed_snapshot: Option<bool>,
    pub auto_create_statistics: Option<bool>,
    pub auto_update_statistics: Option<bool>,
    pub auto_update_statistics_async: Option<bool>,
    pub forced_parameterization: Option<bool>,
    /// The native catalog enablement flag, not Query Store's actual/desired capture mode or retention.
    pub query_store_enabled: Option<bool>,
    pub page_verify: DatabasePropertyCode,
    pub recovery_model: DatabasePropertyCode,
    pub target_recovery_time_seconds: Option<u32>,
}
impl DatabasePropertiesObservation {
    pub fn is_valid(&self) -> bool {
        valid_native_text(&self.database)
            && canonical_positive(&self.native_database_id)
            && self.native_database_id.parse::<i32>().is_ok()
            && self.observed_at.len() <= 64
            && ObservationBounds::new(&self.observed_at, &self.observed_at).is_some()
            && [
                &self.catalog_state,
                &self.user_access,
                &self.snapshot_isolation,
                &self.page_verify,
                &self.recovery_model,
            ]
            .into_iter()
            .all(|value| value.is_valid())
            && self.updateability.as_deref().is_none_or(valid_native_text)
            && self.collation.as_deref().is_none_or(valid_native_text)
            && self.target_recovery_time_seconds.is_none_or(|value| i32::try_from(value).is_ok())
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(self.profile, &self.database, &self.native_database_id, &self.observed_at))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }

    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        self.is_valid()
            && context.is_valid()
            && context
                .target
                .database()
                .is_some_and(|database| database.engine == crate::Engine::Mssql && database.database == self.database)
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
            && context.database_writable.zip(self.writable()).is_none_or(|(scope, point)| scope == point)
    }

    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        source.observation.snapshot().is_some_and(|header| {
            header.dataset == SnapshotDataset::Operational
                && header
                    .sources
                    .iter()
                    .any(|coverage| coverage.source == DATABASE_PROPERTIES_SOURCE && coverage.records == 1)
        }) && NativeReadContext::from_snapshot(source).is_ok_and(|context| self.matches_context(&context))
    }

    /// Native updateability describes this connected database at the point; it is not a general primary-role proof.
    pub fn writable(&self) -> Option<bool> {
        match (self.profile, self.updateability.as_deref()) {
            (DatabasePropertiesProfile::MssqlSysDatabases, Some("READ_WRITE")) => Some(true),
            (DatabasePropertiesProfile::MssqlSysDatabases, Some("READ_ONLY")) => Some(false),
            _ => None,
        }
    }

    pub fn state(&self, kind: DatabasePropertyCodeKind) -> Option<DatabasePropertyState> {
        use DatabasePropertyCodeKind as K;
        use DatabasePropertyState as S;
        // Documented source-native tinyint codes, never engine names or display descriptions.
        // https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-databases-transact-sql
        match self.profile {
            DatabasePropertiesProfile::MssqlSysDatabases => match kind {
                K::CatalogState => match self.catalog_state.code? {
                    0 => Some(S::Online),
                    1 => Some(S::Restoring),
                    2 => Some(S::Recovering),
                    3 => Some(S::RecoveryPending),
                    4 => Some(S::Suspect),
                    5 => Some(S::Emergency),
                    6 => Some(S::Offline),
                    7 => Some(S::Copying),
                    10 => Some(S::OfflineSecondary),
                    _ => None,
                },
                K::UserAccess => match self.user_access.code? {
                    0 => Some(S::MultiUser),
                    1 => Some(S::SingleUser),
                    2 => Some(S::RestrictedUser),
                    _ => None,
                },
                K::SnapshotIsolation => match self.snapshot_isolation.code? {
                    0 => Some(S::Off),
                    1 => Some(S::On),
                    2 => Some(S::TransitionToOff),
                    3 => Some(S::TransitionToOn),
                    _ => None,
                },
                K::PageVerify => match self.page_verify.code? {
                    0 => Some(S::NoVerification),
                    1 => Some(S::TornPageDetection),
                    2 => Some(S::Checksum),
                    _ => None,
                },
                K::RecoveryModel => match self.recovery_model.code? {
                    1 => Some(S::FullRecovery),
                    2 => Some(S::BulkLoggedRecovery),
                    3 => Some(S::SimpleRecovery),
                    _ => None,
                },
            },
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DatabasePropertiesRead {
    pub target: CollectionTarget,
    pub context: ObservationReadContext,
    pub source: StateSourceCoverage,
    pub observation: Option<Box<DatabasePropertiesObservation>>,
    pub read_complete: bool,
}
impl DatabasePropertiesRead {
    pub fn is_valid(&self) -> bool {
        let Some(database) = self.target.database() else { return false };
        let count = u32::from(self.observation.is_some());
        self.target.is_valid()
            && self.context.is_valid()
            && self.source.state != crate::SourceState::Empty
            && self.context.native().is_none_or(|context| context.target == self.target)
            && self.source.source == DATABASE_PROPERTIES_SOURCE
            && self.source.is_valid()
            && self.source.records <= 1
            && count <= self.source.records
            && (!self.read_complete || count == self.source.records)
            && self.observation.as_ref().is_none_or(|point| {
                point.is_valid()
                    && point.database == database.database
                    && database.engine == crate::Engine::Mssql
                    && matches!(self.source.state, crate::SourceState::Collected | crate::SourceState::Partial)
                    && self.context.native().is_none_or(|context| point.matches_context(context))
            })
    }

    pub fn from_source(
        source: &CollectionDescription,
        rows: Vec<OperationalObservation>,
        read_complete: bool,
    ) -> Result<Self> {
        let header = source.observation.snapshot().ok_or_else(invalid)?;
        if header.dataset != SnapshotDataset::Operational {
            return Err(invalid());
        }
        let coverage =
            header.sources.iter().find(|row| row.source == DATABASE_PROPERTIES_SOURCE).ok_or_else(invalid)?.clone();
        let mut observation = None;
        for row in rows.into_iter().filter(|row| row.source == DATABASE_PROPERTIES_SOURCE) {
            if !row.is_valid() || observation.is_some() {
                return Err(invalid());
            }
            let OperationalFact::DatabaseProperties(point) = row.fact else { return Err(invalid()) };
            observation = Some(point);
        }
        let context = ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(source)?) };
        let read = Self { target: source.target.clone(), context, source: coverage, observation, read_complete };
        read.is_valid().then_some(read).ok_or_else(invalid)
    }
}

fn valid_native_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_NATIVE_TEXT_BYTES
}
fn canonical_positive(value: &str) -> bool {
    value.parse::<u32>().is_ok_and(|parsed| parsed > 0 && parsed.to_string() == value)
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
