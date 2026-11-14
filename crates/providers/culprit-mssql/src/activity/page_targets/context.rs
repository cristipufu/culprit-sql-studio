//! Page capability is separate from the shared native database authority.
use super::super::{
    ActivityFrame, ActivityReader,
    database_context::{self, ContextDetail, DatabaseContext},
};
use crate::read::{NativeRead, boolean};
use culprit_core::{
    Result,
    telemetry::{NativeIdentity, PageTargetIdentityBasis, PageWaitResolution},
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct LookupContext {
    pub authority: DatabaseContext,
    pub unavailable: Option<PageWaitResolution>,
}
impl LookupContext {
    pub(super) fn matches_initial(&self, frame: &ActivityFrame) -> bool {
        self.authority.matches_frame(&frame.context)
    }
    pub(super) fn same_source(&self, other: &Self) -> bool {
        self.authority.same_source(&other.authority) && self.unavailable == other.unavailable
    }
    pub(super) fn same_observed_source(&self, other: &Self) -> bool {
        self.authority.same_observed_source(&other.authority) && self.unavailable == other.unavailable
    }
    pub(super) fn database_identity(&self) -> Result<(PageTargetIdentityBasis, NativeIdentity)> {
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
}

pub(super) async fn initial<R: NativeRead>(reader: &mut ActivityReader<R>) -> Result<LookupContext> {
    let (authority, row) = database_context::read_initial(reader, ContextDetail::PageLookup).await?;
    qualify(authority, &row)
}
pub(super) async fn read<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    initial: &LookupContext,
) -> Result<LookupContext> {
    let (authority, row) = database_context::read_again(reader, &initial.authority, ContextDetail::PageLookup).await?;
    qualify(authority, &row)
}
fn qualify(authority: DatabaseContext, row: &crate::value::Rec) -> Result<LookupContext> {
    if !authority.database_wide {
        return Err(super::super::ActivityContext::visibility_error(row, &authority.activity)?);
    }
    let supported = match authority.activity.edition {
        5 => true,
        2 | 3 | 4 | 8 => authority.activity.major >= 15,
        _ => false,
    };
    let unavailable = if !supported || !boolean(row, "page_info_available")? {
        Some(PageWaitResolution::Unsupported)
    } else if let Some(error) = authority.qualification_error {
        Some(super::error_resolution(error))
    } else if !boolean(row, "page_permission")? {
        Some(PageWaitResolution::PermissionDenied)
    } else {
        None
    };
    Ok(LookupContext { authority, unavailable })
}
