//! Native subject resolution is shared by current-state capture and its final consistency check.
use super::description;
use crate::{
    instance,
    read::{NativeRead, invalid_data},
    resources::pool,
};
use culprit_core::{DatabaseIdentity, Error, ErrorCode, Result, telemetry::*};

#[derive(Clone, PartialEq)]
pub(super) enum Identity {
    Database { database: DatabaseIdentity, physical: String },
    Instance(instance::Context),
    Pool(pool::Context),
}
impl Identity {
    pub fn target(&self) -> CollectionTarget {
        match self {
            Self::Database { database, .. } => database.clone().into(),
            Self::Instance(context) => CollectionTarget::Shared(context.identity.clone()),
            Self::Pool(context) => CollectionTarget::Shared(context.identity.clone()),
        }
    }
    pub fn azure_guarded(&self) -> bool {
        !matches!(self, Self::Instance(_))
    }
    pub async fn recheck<R: NativeRead>(&self, reader: &mut R, host: &str, port: u16) -> Result<Self> {
        match self {
            Self::Database { .. } => {
                let (database, physical) = description::identity(reader, host, port).await?;
                Ok(Self::Database { database, physical })
            }
            Self::Instance(_) => Ok(Self::Instance(instance::read(reader).await?.0)),
            Self::Pool(_) => Ok(Self::Pool(
                pool::identity(reader).await?.0.pool.ok_or_else(|| CollectionStop::SourceChanged.error())?,
            )),
        }
    }
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
    if (matches!(subject, CollectionSubject::Shared { resource } if resource.kind == SharedResourceKind::Pool)
        && dataset != SnapshotDataset::MemoryFailures)
        || (dataset == SnapshotDataset::MemoryFailures && matches!(subject, CollectionSubject::Database))
    {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    Ok(())
}

pub(super) async fn read<R: NativeRead>(
    reader: &mut R,
    subject: &CollectionSubject,
    host: &str,
    port: u16,
) -> Result<Identity> {
    is_shared(subject)?;
    let identity = match subject {
        CollectionSubject::Database => {
            description::ensure_supported(reader).await?;
            let (database, physical) = description::identity(reader, host, port).await?;
            Identity::Database { database, physical }
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
    Ok(identity)
}
