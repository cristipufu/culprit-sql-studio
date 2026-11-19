//! Page capability is separate from the shared native database authority.
use super::super::{
    ActivityFrame, ActivityReader,
    database_context::{self, Context, Detail},
};
use crate::read::{NativeRead, boolean};
use culprit_core::{
    Result,
    telemetry::{CollectionStop, NativeIdentity, PageTargetIdentityBasis, PageWaitResolution, SourcePartition},
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct LookupContext {
    pub authority: Context,
    pub unavailable: Option<PageWaitResolution>,
    /// Population visibility is not a database epoch or an absence proof. Focused coordinator reads set None.
    pub task_visibility: Option<bool>,
    pub shared: Option<super::shared_tempdb::Context>,
    pub shared_requested: bool,
    pub shared_only: bool,
    page_info_available: bool,
}
impl LookupContext {
    pub(super) fn supports_primary_tempdb_file(&self) -> bool {
        matches!(self.authority.activity.edition, 2..=4)
            && self.authority.activity.major >= 15
            && self.page_info_available
            && self.authority.is_qualified()
            && self.authority.connection_generation().is_some()
            && self.unavailable().is_none()
    }
    pub(super) fn matches_initial(&self, frame: &ActivityFrame) -> bool {
        self.authority.matches_frame(&frame.context)
    }
    pub(super) fn same_source(&self, other: &Self) -> bool {
        self.authority.same_source(&other.authority)
            && self.unavailable == other.unavailable
            && self.same_shared_source(other)
    }
    pub(super) fn same_observed_source(&self, other: &Self) -> bool {
        self.authority.same_observed_source(&other.authority)
            && self.unavailable == other.unavailable
            && self.same_shared_source(other)
    }
    fn same_shared_source(&self, other: &Self) -> bool {
        match (&self.shared, &other.shared) {
            (None, None) => true,
            (Some(before), Some(after)) => before.same_source(after),
            _ => false,
        }
    }
    pub(super) fn database_identity(
        &self,
        address: super::PageAddress,
    ) -> Result<(PageTargetIdentityBasis, NativeIdentity)> {
        if let Some(shared) = self.shared_for(address) {
            return Ok((PageTargetIdentityBasis::LocalDatabaseFileEpoch, shared.database_identity()?));
        }
        let basis = if self.authority.activity.edition == 5 {
            PageTargetIdentityBasis::DatabaseIncarnation
        } else {
            PageTargetIdentityBasis::LocalDatabaseFileEpoch
        };
        Ok((basis, self.authority.native_identity()?))
    }
    pub(super) fn capture_context(&self) -> Result<super::ActivityContext> {
        self.authority.capture_context()
    }
    pub(super) fn unavailable(&self) -> Option<PageWaitResolution> {
        self.unavailable.or_else(|| (!self.authority.is_qualified()).then_some(PageWaitResolution::UnverifiedSource))
    }
    pub(super) fn shared_for(&self, address: super::PageAddress) -> Option<&super::shared_tempdb::Context> {
        self.shared.as_ref().filter(|_| super::shared_tempdb::Context::accepts(address))
    }
    pub(super) fn accepts(&self, address: super::PageAddress) -> bool {
        (self.shared_requested && super::shared_tempdb::Context::accepts(address))
            || (!self.shared_only && address.database == self.authority.database_id)
    }
    pub(super) fn unavailable_for(&self, address: super::PageAddress) -> Option<PageWaitResolution> {
        if self.shared_requested && super::shared_tempdb::Context::accepts(address) {
            return self.unavailable.filter(|reason| *reason == PageWaitResolution::Unsupported).or_else(|| {
                self.shared.as_ref().map_or(Some(PageWaitResolution::UnverifiedSource), |shared| shared.unavailable())
            });
        }
        self.unavailable()
    }
    pub(super) fn observed_until(&self) -> &str {
        self.shared.as_ref().map_or(self.authority.activity.at.as_str(), |shared| {
            shared.observed_until().max(self.authority.activity.at.as_str())
        })
    }
}

pub(super) async fn initial<R: NativeRead>(reader: &mut ActivityReader<R>) -> Result<LookupContext> {
    let (authority, row) = database_context::read_initial(reader, Detail::PageLookup).await?;
    qualify(authority, &row)
}
pub(super) async fn initial_shared<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    resource: &culprit_core::telemetry::SharedResourceIdentity,
) -> Result<LookupContext> {
    let (authority, row) = database_context::read_initial(reader, Detail::PageLookup).await?;
    shared_context(reader, resource, authority, row).await
}
pub(super) async fn initial_shared_focus<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    resource: &culprit_core::telemetry::SharedResourceIdentity,
    expected: &SourcePartition,
) -> Result<LookupContext> {
    let (authority, row) = database_context::read_initial(reader, Detail::PageLookup).await?;
    let instance = authority.instance.as_ref().ok_or_else(|| CollectionStop::SourceChanged.error())?;
    let current = SourcePartition::InstanceLocal {
        member: Some(instance.identity.clone()),
        epoch: Some(instance.capture_epoch()?),
    };
    if current != *expected {
        return Err(CollectionStop::SourceChanged.error());
    }
    shared_context(reader, resource, authority, row).await
}
async fn shared_context<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    resource: &culprit_core::telemetry::SharedResourceIdentity,
    authority: Context,
    row: crate::value::Rec,
) -> Result<LookupContext> {
    let shared = super::shared_tempdb::Context::initial(
        &mut reader.io,
        &authority,
        resource,
        boolean(&row, "page_info_available")?,
    )
    .await?;
    let mut context = qualify(authority, &row)?;
    context.shared = Some(shared);
    context.shared_requested = true;
    context.shared_only = true;
    Ok(context)
}
pub(super) async fn initial_scan<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    include_shared: bool,
) -> Result<LookupContext> {
    let mut context = initial(reader).await?;
    context.shared_requested = include_shared;
    if include_shared
        && context.authority.is_qualified()
        && context.authority.instance.is_some()
        && supports_page_info(
            context.authority.activity.edition,
            context.authority.activity.major,
            context.page_info_available,
        )
        && let Some(resource) = context.authority.source_instance()
    {
        context.shared = Some(
            super::shared_tempdb::Context::initial(
                &mut reader.io,
                &context.authority,
                &resource,
                context.page_info_available,
            )
            .await?,
        );
    }
    Ok(context)
}
pub(super) async fn read<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    initial: &LookupContext,
) -> Result<LookupContext> {
    let (authority, row) = database_context::read_again(reader, &initial.authority, Detail::PageLookup).await?;
    let shared = match &initial.shared {
        Some(shared) => Some(shared.recheck(&mut reader.io, &authority).await?),
        None => None,
    };
    let mut context = qualify(authority, &row)?;
    context.shared = shared;
    context.shared_requested = initial.shared_requested;
    context.shared_only = initial.shared_only;
    Ok(context)
}
fn qualify(authority: Context, row: &crate::value::Rec) -> Result<LookupContext> {
    if !authority.database_wide {
        return Err(super::super::ActivityContext::visibility_error(row, &authority.activity)?);
    }
    let page_info_available = boolean(row, "page_info_available")?;
    let unavailable = if !supports_page_info(authority.activity.edition, authority.activity.major, page_info_available)
    {
        Some(PageWaitResolution::Unsupported)
    } else if let Some(error) = authority.qualification_error {
        Some(super::error_resolution(error))
    } else if !boolean(row, "page_permission")? {
        Some(PageWaitResolution::PermissionDenied)
    } else {
        None
    };
    Ok(LookupContext {
        authority,
        unavailable,
        task_visibility: None,
        shared: None,
        shared_requested: false,
        shared_only: false,
        page_info_available,
    })
}

/// Managed service product versions do not identify their storage-engine feature generation. Require the
/// observed function on every route; boxed SQL Server additionally needs its documented 2019+ native layout.
pub(super) fn supports_page_info(edition: i64, major: u32, available: bool) -> bool {
    available
        && match edition {
            5 | 8 => true,
            2..=4 => major >= 15,
            _ => false,
        }
}
