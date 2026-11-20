//! Reuses existing native authority owners. Logical backup history does not manufacture a compute member.
use crate::{
    database_identity, instance, local_source,
    read::{NativeRead, invalid_data, require_primary},
};
use culprit_core::{DatabaseIdentity, Error, ErrorCode, Result, telemetry::*};

pub(crate) enum Authority {
    Azure { database: database_identity::Context, major: u32, at: String, generation: u64, host: String, port: u16 },
    Local(Box<local_source::Context>),
}
impl Authority {
    pub(crate) async fn read(reader: &mut impl NativeRead, host: &str, port: u16) -> Result<Self> {
        let platform = instance::observe(reader).await?;
        let generation = reader
            .connection_generation()
            .filter(|generation| *generation > 0)
            .ok_or_else(|| CollectionStop::SourceUnavailable.error())?;
        let value = if platform.edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            let database = database_identity::read_azure(reader).await?;
            require_primary(database.writable)?;
            Self::Azure {
                database,
                major: platform.major,
                at: platform.observed_at,
                generation,
                host: host.into(),
                port,
            }
        } else {
            if !instance::supports_profile(platform.edition, platform.major) {
                return Err(unsupported());
            }
            let context = local_source::Context::read(reader, host, port).await?;
            if context.activity.edition != platform.edition
                || context.activity.major != platform.major
                || !context.is_qualified()
                || !context.source_partition()?.is_qualified()
                || context.connection_generation() != Some(generation)
                || ObservationBounds::new(&platform.observed_at, &context.activity.at).is_none()
            {
                return Err(CollectionStop::SourceChanged.error());
            }
            Self::Local(Box::new(context))
        };
        if reader.connection_generation() != Some(generation) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(value)
    }
    pub(crate) async fn recheck(&self, reader: &mut impl NativeRead, host: &str, port: u16) -> Result<Self> {
        match self {
            Self::Azure { .. } => Self::read(reader, host, port).await,
            Self::Local(original) => {
                let after = original.recheck(reader, host, port).await?;
                if !original.same_source(&after)
                    || original.completed(&after).is_err()
                    || after.connection_generation().is_none_or(|generation| generation == 0)
                {
                    return Err(CollectionStop::SourceChanged.error());
                }
                Ok(Self::Local(Box::new(after)))
            }
        }
    }
    pub(crate) fn same_source(&self, after: &Self) -> bool {
        match (self, after) {
            (
                Self::Azure { database: a, major: am, generation: ag, .. },
                Self::Azure { database: b, major: bm, generation: bg, .. },
            ) => a == b && am == bm && ag == bg,
            (Self::Local(a), Self::Local(b)) => a.same_source(b),
            _ => false,
        }
    }
    pub(crate) fn profile(&self) -> MssqlBackupHistoryProfile {
        match self {
            Self::Azure { .. } => MssqlBackupHistoryProfile::AzureSqlDatabase,
            Self::Local(context) if context.activity.edition == crate::probe::ENGINE_EDITION_MANAGED_INSTANCE => {
                MssqlBackupHistoryProfile::ManagedInstanceMsdb
            }
            Self::Local(_) => MssqlBackupHistoryProfile::SqlServerMsdb,
        }
    }
    pub(crate) fn database(&self) -> DatabaseIdentity {
        match self {
            Self::Azure { database, host, port, .. } => database.identity(host, *port),
            Self::Local(context) => context.activity.database.clone(),
        }
    }
    pub(crate) fn partition(&self) -> Result<SourcePartition> {
        match self {
            Self::Azure { .. } => Ok(SourcePartition::Target),
            Self::Local(context) => context.source_partition(),
        }
    }
    pub(crate) fn writable(&self) -> Result<bool> {
        match self {
            Self::Azure { database, .. } => database.writable,
            Self::Local(context) => context.activity.writable,
        }
        .ok_or_else(invalid_data)
    }
    pub(crate) fn at(&self) -> &str {
        match self {
            Self::Azure { at, .. } => at,
            Self::Local(context) => &context.activity.at,
        }
    }
    pub(crate) fn edition(&self) -> i64 {
        match self {
            Self::Azure { .. } => 5,
            Self::Local(context) => context.activity.edition,
        }
    }
    pub(crate) fn major(&self) -> u32 {
        match self {
            Self::Azure { major, .. } => *major,
            Self::Local(context) => context.activity.major,
        }
    }
    pub(crate) fn scope(&self, database_id: u32, hyperscale: Option<bool>) -> Result<MssqlBackupHistoryScope> {
        let native_database = match self {
            Self::Azure { database, .. } => {
                NativeIdentity::new(database.incarnation.as_ref().ok_or_else(invalid_data)?)
                    .qualified("kind", "azure-sql-logical-database")
                    .qualified("physical_database_guid", database.physical_incarnation().ok_or_else(invalid_data)?)
            }
            Self::Local(context) => {
                if context.database_id != database_id {
                    return Err(CollectionStop::SourceChanged.error());
                }
                context.native_identity()?
            }
        };
        Ok(MssqlBackupHistoryScope { profile: self.profile(), database_id, native_database, hyperscale })
    }
    pub(crate) fn database_guid(&self) -> Result<&str> {
        match self {
            Self::Azure { database, .. } => database.incarnation.as_deref(),
            Self::Local(context) => context.database_guid.as_deref(),
        }
        .ok_or_else(invalid_data)
    }
    pub(crate) fn recovery_fork(&self) -> Option<&str> {
        match self {
            Self::Azure { .. } => None,
            Self::Local(context) => context.recovery_fork_guid.as_deref(),
        }
    }
}
fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))
}
