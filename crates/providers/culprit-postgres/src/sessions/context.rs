use crate::{native, read::QueryRead};
use culprit_core::{ErrorCode, Result, telemetry::*};

pub(crate) struct Context {
    metadata: native::metadata::Metadata,
    member: SharedResourceIdentity,
}

impl Context {
    pub(crate) async fn observe<R: QueryRead>(
        reader: &mut R,
        metadata: &native::metadata::Metadata,
    ) -> Result<Option<Self>> {
        let member = match native::instance::read(reader, metadata.version).await {
            Ok(instance) => instance.identity,
            Err(error) if optional(&error) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        Ok(Some(Self { metadata: metadata.clone(), member }))
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
        if !self.metadata.same_activity_source(&after) {
            return Ok(None);
        }
        let Some(other) = Self::observe(reader, &after).await? else { return Ok(None) };
        if self.member != other.member {
            return Ok(None);
        }
        let context = NativeReadContext {
            target: self.metadata.database.clone().into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(self.member.clone()),
                epoch: Some(self.metadata.activity_epoch()),
            },
            observed_from: canonical_time(&self.metadata.at)?,
            observed_until: canonical_time(&after.at)?,
            database_writable: Some(!self.metadata.standby),
        };
        Ok(context.has_qualified_local_owner().then_some(context))
    }
}

fn optional(error: &culprit_core::Error) -> bool {
    (error.detail.is_none() && matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported))
        || error.detail == Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
}
