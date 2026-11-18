use crate::{native, read::QueryRead};
use culprit_core::{ErrorCode, Result, telemetry::*};

pub(crate) struct Context {
    metadata: native::metadata::Metadata,
    member: SharedResourceIdentity,
    member_observed_at: String,
}

pub(crate) struct Completion {
    pub(crate) context: NativeReadContext,
    pub(crate) full_visibility: bool,
}

impl Context {
    pub(crate) async fn observe<R: QueryRead>(
        reader: &mut R,
        metadata: &native::metadata::Metadata,
    ) -> Result<Option<Self>> {
        let instance = match native::instance::read(reader, metadata.version).await {
            Ok(instance) => instance,
            Err(error) if optional(&error) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if ObservationBounds::new(&metadata.at, &instance.observed_at).is_none() {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(Some(Self {
            metadata: metadata.clone(),
            member: instance.identity,
            member_observed_at: instance.observed_at,
        }))
    }

    pub(crate) fn read_context(&self) -> Result<NativeReadContext> {
        self.context_until(&self.member_observed_at)
    }

    pub(crate) async fn recheck<R: QueryRead>(
        &self,
        reader: &mut R,
        host: &str,
        port: u16,
    ) -> Result<Option<NativeReadContext>> {
        Ok(self.recheck_completion(reader, host, port).await?.map(|completion| completion.context))
    }

    async fn recheck_completion<R: QueryRead>(
        &self,
        reader: &mut R,
        host: &str,
        port: u16,
    ) -> Result<Option<Completion>> {
        let after = match native::metadata::activity(reader, host, port).await {
            Ok(metadata) => metadata,
            Err(error) if optional(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        if !self.metadata.same_activity_source(&after) || ObservationBounds::new(&self.metadata.at, &after.at).is_none()
        {
            return Ok(None);
        }
        let Some(other) = Self::observe(reader, &after).await? else { return Ok(None) };
        if self.member != other.member {
            return Ok(None);
        }
        let context = self.context_until(&other.member_observed_at)?;
        Ok(context
            .has_qualified_local_owner()
            .then_some(Completion { context, full_visibility: self.metadata.full_visibility && after.full_visibility }))
    }

    fn context_until(&self, until: &str) -> Result<NativeReadContext> {
        let context = NativeReadContext {
            target: self.metadata.database.clone().into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(self.member.clone()),
                epoch: Some(self.metadata.activity_epoch()),
            },
            observed_from: canonical_time(&self.metadata.at)?,
            observed_until: canonical_time(until)?,
            database_writable: Some(self.metadata.writable_role()),
        };
        if !context.is_valid() {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(context)
    }
}

pub(crate) fn unknown(metadata: &native::metadata::Metadata, until: &str) -> Result<NativeReadContext> {
    let context = NativeReadContext {
        target: metadata.database.clone().into(),
        partition: SourcePartition::InstanceLocal { member: None, epoch: Some(metadata.activity_epoch()) },
        observed_from: canonical_time(&metadata.at)?,
        observed_until: canonical_time(until)?,
        database_writable: Some(metadata.writable_role()),
    };
    if !context.is_valid() {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(context)
}

/// Complete the same held read. A later permission grant cannot fill missing initial member authority.
pub(crate) async fn complete<R: QueryRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    before: &native::metadata::Metadata,
    owner: Option<&Context>,
) -> Result<Completion> {
    if let Some(owner) = owner {
        return owner
            .recheck_completion(reader, host, port)
            .await?
            .ok_or_else(|| CollectionStop::SourceChanged.error());
    }
    let after = native::metadata::activity(reader, host, port).await?;
    if !before.same_activity_source(&after) || ObservationBounds::new(&before.at, &after.at).is_none() {
        return Err(CollectionStop::SourceChanged.error());
    }
    let until = match Context::observe(reader, &after).await? {
        Some(owner) => owner.read_context()?.observed_until,
        None => after.at,
    };
    Ok(Completion {
        context: unknown(before, &until)?,
        full_visibility: before.full_visibility && after.full_visibility,
    })
}

fn optional(error: &culprit_core::Error) -> bool {
    (error.detail.is_none() && matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported))
        || error.detail == Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
}
