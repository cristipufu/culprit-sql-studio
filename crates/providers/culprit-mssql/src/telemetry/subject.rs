//! Native subject resolution is shared by current-state capture and its final consistency check.
use super::description;
use crate::{
    instance, local_source,
    read::{NativeRead, invalid_data},
    resources::pool,
};
use culprit_core::{DatabaseIdentity, Error, ErrorCode, Result, telemetry::*};

#[derive(Clone, PartialEq)]
pub(super) enum Identity {
    Database { database: DatabaseIdentity, physical: String },
    LocalDatabase(Box<local_source::Context>),
    Instance(instance::Context),
    Pool(pool::Context),
    PoolCurrent(Box<pool::CurrentContext>),
    ResourcePoolSettings(Box<pool::SettingsContext>),
}
impl Identity {
    pub fn target(&self) -> CollectionTarget {
        match self {
            Self::Database { database, .. } => database.clone().into(),
            Self::LocalDatabase(context) => context.activity.database.clone().into(),
            Self::Instance(context) => CollectionTarget::Shared(context.identity.clone()),
            Self::Pool(context) => CollectionTarget::Shared(context.identity.clone()),
            Self::PoolCurrent(context) => CollectionTarget::Shared(context.point.pool.clone()),
            Self::ResourcePoolSettings(context) => context.target.clone(),
        }
    }
    pub fn partition(&self) -> Result<SourcePartition> {
        Ok(match self {
            Self::LocalDatabase(context) => context.source_partition()?,
            Self::Instance(context) => SourcePartition::InstanceLocal {
                member: Some(context.identity.clone()),
                epoch: Some(context.capture_epoch()?),
            },
            Self::Database { .. } | Self::Pool(_) | Self::PoolCurrent(_) | Self::ResourcePoolSettings(_) => {
                SourcePartition::Target
            }
        })
    }
    pub async fn admit<R: NativeRead>(&self, reader: &mut R) -> Result<()> {
        match self {
            Self::LocalDatabase(_) => crate::resources::admit_instance_resources(reader).await,
            Self::Database { .. } | Self::Pool(_) | Self::PoolCurrent(_) | Self::ResourcePoolSettings(_) => {
                crate::resources::admit_resources(reader).await
            }
            Self::Instance(_) => return Ok(()),
        }
        .map_err(crate::read::ReadError::into_error)
    }
    pub fn accepts_observation_bounds(&self, from: &str, until: &str) -> bool {
        match self {
            Self::PoolCurrent(context) => {
                ObservationBounds::new(&context.point.observed_at, from).is_some()
                    && ObservationBounds::new(from, until).is_some()
            }
            Self::LocalDatabase(context) => {
                ObservationBounds::new(&context.activity.at, from).is_some()
                    && ObservationBounds::new(from, until).is_some()
            }
            _ => true,
        }
    }
    pub fn confirmed_by(&self, after: &Self, observed_until: &str) -> bool {
        match (self, after) {
            (Self::PoolCurrent(before), Self::PoolCurrent(after)) => before.confirms(after, observed_until),
            (Self::ResourcePoolSettings(original), Self::ResourcePoolSettings(after)) => {
                original.confirmed_by(after, observed_until)
            }
            (Self::LocalDatabase(original), Self::LocalDatabase(after)) => {
                require_local_database(after).is_ok()
                    && original.same_source(after)
                    && original.completed(after).is_ok()
                    && ObservationBounds::new(observed_until, &after.activity.at).is_some()
            }
            _ => self == after,
        }
    }
    /// Preserve the already-paid instance observation clock for profiles requiring a final UTC boundary.
    pub async fn recheck_clock<R: NativeRead>(
        &self,
        reader: &mut R,
        host: &str,
        port: u16,
    ) -> Result<(Self, Option<String>)> {
        if matches!(self, Self::Instance(_)) {
            let (identity, at) = instance::read(reader).await?;
            return Ok((Self::Instance(identity), Some(at)));
        }
        self.recheck(reader, host, port).await.map(|identity| (identity, None))
    }
    pub async fn recheck<R: NativeRead>(&self, reader: &mut R, host: &str, port: u16) -> Result<Self> {
        match self {
            Self::PoolCurrent(original) => Ok(Self::PoolCurrent(Box::new(original.recheck(reader).await?))),
            Self::ResourcePoolSettings(original) => {
                Ok(Self::ResourcePoolSettings(Box::new(original.recheck(reader).await?)))
            }
            Self::Database { .. } => {
                let (database, physical) = description::identity(reader, host, port).await?;
                Ok(Self::Database { database, physical })
            }
            Self::LocalDatabase(original) => {
                Ok(Self::LocalDatabase(Box::new(original.recheck(reader, host, port).await?)))
            }
            Self::Instance(_) => Ok(Self::Instance(instance::read(reader).await?.0)),
            Self::Pool(_) => Ok(Self::Pool(
                pool::identity(reader).await?.0.pool.ok_or_else(|| CollectionStop::SourceChanged.error())?,
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum DatabaseAuthority {
    AzureOnly,
    LocalDatabase,
}
impl DatabaseAuthority {
    pub fn for_source(dataset: SnapshotDataset, subject: &CollectionSubject) -> Result<Self> {
        if !matches!(
            dataset,
            SnapshotDataset::Tuning
                | SnapshotDataset::Settings
                | SnapshotDataset::DatabasePerformanceCounters
                | SnapshotDataset::SuspectPages
        ) {
            return Ok(Self::AzureOnly);
        }
        if matches!(subject, CollectionSubject::Shared { resource } if resource.kind == SharedResourceKind::Pool)
            && dataset == SnapshotDataset::Settings
        {
            return Ok(Self::AzureOnly);
        }
        subject.require_database()?;
        Ok(Self::LocalDatabase)
    }
    async fn read<R: NativeRead>(
        self,
        reader: &mut R,
        host: &str,
        port: u16,
        detail: crate::database_identity::log_rates::Detail,
    ) -> Result<SubjectRead> {
        let profile = match self {
            Self::AzureOnly => description::supported_profile(reader, detail).await?,
            Self::LocalDatabase => description::observed_profile(reader, detail).await?,
        };
        if profile.edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            let generation = reader.connection_generation();
            let context = local_source::Context::read(reader, host, port).await?;
            require_local_database(&context)?;
            if generation != context.connection_generation() || profile.edition != context.activity.edition {
                return Err(CollectionStop::SourceChanged.error());
            }
            return Ok(SubjectRead {
                identity: Identity::LocalDatabase(Box::new(context)),
                configured_log_rates: Ok(None),
            });
        }
        // Keep Azure's original GUID, physical incarnation and primary-role resolver. The profile
        // probe above replaces no authority and the local branch cannot supply a missing Azure GUID.
        let projection = if profile.configured_log_rates.as_ref().is_ok_and(|available| *available) {
            detail
        } else {
            crate::database_identity::log_rates::Detail::Identity
        };
        let read = description::identity_detail(reader, host, port, projection).await?;
        Ok(SubjectRead {
            identity: Identity::Database { database: read.database, physical: read.physical_incarnation },
            configured_log_rates: match profile.configured_log_rates {
                Ok(_) => read.configured_log_rates,
                Err(error) => Err(error),
            },
        })
    }
}

fn require_local_database(context: &local_source::Context) -> Result<()> {
    if context.activity.writable != Some(true)
        || !matches!(context.source_instance(), Some(member) if member.kind == SharedResourceKind::Instance)
    {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    if !context.is_qualified() || context.connection_generation().is_none_or(|generation| generation == 0) {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(())
}

pub(super) fn is_shared(subject: &CollectionSubject) -> Result<bool> {
    match subject {
        CollectionSubject::Database => Ok(false),
        CollectionSubject::Shared { resource }
            if resource.engine == culprit_core::Engine::Mssql
                && matches!(resource.kind, SharedResourceKind::Instance | SharedResourceKind::Pool) =>
        {
            Ok(true)
        }
        _ => Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
    }
}

pub(super) fn validate_dataset(subject: &CollectionSubject, dataset: SnapshotDataset) -> Result<()> {
    is_shared(subject)?;
    if matches!(dataset, SnapshotDataset::NumaNodes | SnapshotDataset::ProcessCpu)
        && !matches!(subject,CollectionSubject::Shared{resource} if resource.engine==culprit_core::Engine::Mssql && resource.kind==SharedResourceKind::Instance)
    {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    if (matches!(subject, CollectionSubject::Shared { resource } if resource.kind == SharedResourceKind::Pool)
        && !matches!(
            dataset,
            SnapshotDataset::MemoryFailures | SnapshotDataset::Settings | SnapshotDataset::WaitCounters
        ))
        || (dataset == SnapshotDataset::MemoryFailures && matches!(subject, CollectionSubject::Database))
    {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    Ok(())
}

pub(super) struct SubjectRead {
    pub identity: Identity,
    pub configured_log_rates: Result<Option<ConfiguredLogRateLimits>>,
}

pub(super) async fn read_detail<R: NativeRead>(
    reader: &mut R,
    subject: &CollectionSubject,
    host: &str,
    port: u16,
    detail: crate::database_identity::log_rates::Detail,
    database_authority: DatabaseAuthority,
) -> Result<SubjectRead> {
    is_shared(subject)?;
    let mut configured_log_rates = Ok(None);
    let identity = match subject {
        CollectionSubject::Database => {
            let read = database_authority.read(reader, host, port, detail).await?;
            configured_log_rates = read.configured_log_rates;
            read.identity
        }
        CollectionSubject::Shared { resource } if resource.kind == SharedResourceKind::Instance => {
            Identity::Instance(instance::read(reader).await?.0)
        }
        CollectionSubject::Shared { resource } if resource.kind == SharedResourceKind::Pool => {
            description::ensure_supported(reader).await?;
            Identity::Pool(pool::identity(reader).await?.0.pool.ok_or_else(|| CollectionStop::SourceChanged.error())?)
        }
        _ => return Err(invalid_data()),
    };
    if !subject.accepts(&identity.target()) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(SubjectRead { identity, configured_log_rates })
}
