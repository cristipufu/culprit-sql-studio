//! History data stays query-global. This held native observer owns only acquisition and pressure admission.
use crate::{
    local_source::Context,
    read::{NativeRead, ReadError, invalid_data},
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    AzureLogical,
    BoxedSql2022,
}
#[derive(Clone)]
pub(super) struct HistoryAcquisition {
    profile: Profile,
    native: Box<Context>,
    connection_generation: Option<u64>,
}
impl HistoryAcquisition {
    pub(super) async fn read<R: NativeRead>(reader: &mut R, host: &str, port: u16) -> Result<Self> {
        let context = Context::read(reader, host, port).await?;
        Self::observed(context, reader.connection_generation())
    }
    fn observed(context: Context, connection_generation: Option<u64>) -> Result<Self> {
        let profile = match (context.activity.edition, context.activity.major) {
            (5, _) => Profile::AzureLogical,
            (2..=4, 16) => Profile::BoxedSql2022,
            // Managed Instance and other native versions need their own real compatibility acceptance.
            _ => return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
        };
        if !context.is_qualified() {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        let owner = context.completed(&context)?;
        if !owner.has_qualified_local_owner() || owner.database_writable.is_none() {
            return Err(invalid_data());
        }
        Ok(Self { profile, native: Box::new(context), connection_generation })
    }
    pub(super) fn owner(&self) -> Result<NativeReadContext> {
        self.native.completed(&self.native)
    }
    pub(super) fn target(&self) -> CollectionTarget {
        self.native.activity.database.clone().into()
    }
    pub(super) fn edition(&self) -> i64 {
        self.native.activity.edition
    }
    pub(super) async fn admit<R: NativeRead>(&self, reader: &mut R) -> Result<()> {
        if self.connection_generation != reader.connection_generation() {
            return Err(CollectionStop::SourceChanged.error());
        }
        match self.profile {
            Profile::AzureLogical => crate::resources::admit_resources(reader).await,
            Profile::BoxedSql2022 => crate::resources::admit_instance_resources(reader).await,
        }
        .map_err(ReadError::into_error)
    }
    pub(super) async fn recheck<R: NativeRead>(&self, reader: &mut R, host: &str, port: u16) -> Result<Self> {
        if self.connection_generation != reader.connection_generation() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let after = self.native.recheck(reader, host, port).await?;
        let value = Self::observed(after, reader.connection_generation())?;
        if self.profile != value.profile
            || self.connection_generation != value.connection_generation
            || !self.native.same_source(&value.native)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(value)
    }
    pub(super) fn completed(&self, after: &Self) -> Result<NativeReadContext> {
        let original = self.owner()?;
        if self.profile != after.profile
            || self.connection_generation != after.connection_generation
            || !after.owner()?.matches_earlier_scope(&original)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let completed = self.native.completed(&after.native)?;
        if !completed.completes_read(&original) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(completed)
    }
}

#[cfg(test)]
mod tests;
