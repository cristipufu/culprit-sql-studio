use crate::{native, read::QueryRead};
use culprit_core::{ErrorCode, Result, telemetry::*};

pub(crate) struct Context {
    metadata: native::metadata::Metadata,
    member: SharedResourceIdentity,
    member_observed_at: String,
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
        Ok(context.has_qualified_local_owner().then_some(context))
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
            database_writable: Some(!self.metadata.standby),
        };
        if !context.is_valid() {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(context)
    }
}

fn optional(error: &culprit_core::Error) -> bool {
    (error.detail.is_none() && matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported))
        || error.detail == Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
}
