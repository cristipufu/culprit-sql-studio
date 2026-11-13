use super::super::{ActivityContext, ActivityFrame, ActivityReader, IdentityDetail};
use crate::{
    instance,
    read::{
        NativeRead, Parameter, ReadError, boolean, integer, invalid_data, optional_text, stops_remaining_reads, text,
    },
};
use culprit_core::{
    Result,
    telemetry::{NativeIdentity, PageTargetIdentityBasis, PageWaitResolution, digest},
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct LookupContext {
    pub activity: ActivityContext,
    pub database_id: u32,
    pub database_created: String,
    pub physical_database: Option<String>,
    pub database_file_guid: Option<String>,
    pub database_file_create_lsn: Option<String>,
    pub instance: Option<instance::Context>,
    pub unavailable: Option<PageWaitResolution>,
}
impl LookupContext {
    pub(super) fn matches_initial(&self, frame: &ActivityFrame) -> bool {
        self.activity.edition == frame.context.edition
            && self.activity.major == frame.context.major
            && self.activity.database.database == frame.context.database.database
            && self.activity.writable == frame.context.writable
            && frame.context.epoch.as_ref().is_none_or(|epoch| self.activity.epoch.as_ref() == Some(epoch))
    }

    pub(super) fn same_source(&self, other: &Self) -> bool {
        self.same_observed_source(other) && self.qualified() && other.qualified()
    }

    pub(super) fn same_observed_source(&self, other: &Self) -> bool {
        self.activity.database == other.activity.database
            && self.activity.epoch == other.activity.epoch
            && self.activity.edition == other.activity.edition
            && self.activity.major == other.activity.major
            && self.activity.writable == other.activity.writable
            && self.database_id == other.database_id
            && self.database_created == other.database_created
            && self.physical_database == other.physical_database
            && self.database_file_guid == other.database_file_guid
            && self.database_file_create_lsn == other.database_file_create_lsn
            && self.instance == other.instance
            && self.unavailable == other.unavailable
    }

    fn qualified(&self) -> bool {
        if self.activity.edition == 5 {
            self.activity.database.incarnation.is_some() && self.physical_database.is_some()
        } else {
            self.instance.is_some() && self.database_file_guid.is_some()
        }
    }

    pub(super) fn database_identity(&self) -> Result<(PageTargetIdentityBasis, NativeIdentity)> {
        if self.activity.edition == 5 {
            let logical = self.activity.database.incarnation.as_ref().ok_or_else(invalid_data)?;
            let physical = self.physical_database.as_ref().ok_or_else(invalid_data)?;
            Ok((
                PageTargetIdentityBasis::DatabaseIncarnation,
                NativeIdentity::new(logical)
                    .qualified("kind", "azure-sql-logical-database")
                    .qualified("physical_database_guid", physical),
            ))
        } else {
            let instance = self.instance.as_ref().ok_or_else(invalid_data)?;
            let mut identity = NativeIdentity::new(self.database_id.to_string())
                .qualified("kind", "sql-server-local-database-file-epoch")
                .qualified("database_created_without_offset", &self.database_created)
                .qualified("database_file_guid", self.database_file_guid.as_ref().ok_or_else(invalid_data)?)
                .qualified("instance_epoch", instance.capture_epoch()?);
            // A newly created primary file can have native create_lsn=NULL. The real file GUID and local
            // database/process epoch qualify this boundary; an absent optional LSN never supplies authority.
            if let Some(lsn) = &self.database_file_create_lsn {
                identity = identity.qualified("database_file_create_lsn", lsn);
            }
            Ok((PageTargetIdentityBasis::LocalDatabaseFileEpoch, identity))
        }
    }

    pub(super) fn capture_context(&self) -> Result<ActivityContext> {
        let mut context = self.activity.clone();
        if context.edition != 5 && self.qualified() {
            let (_, identity) = self.database_identity()?;
            context.epoch = Some(digest(&serde_json::to_vec(&identity).map_err(|_| invalid_data())?));
        }
        Ok(context)
    }

    pub(super) fn unavailable(&self) -> Option<PageWaitResolution> {
        self.unavailable.or_else(|| (!self.qualified()).then_some(PageWaitResolution::UnverifiedSource))
    }
}

pub(super) async fn initial<R: NativeRead>(reader: &mut ActivityReader<R>) -> Result<LookupContext> {
    let initial = read_context(reader, reader.options.identity).await?;
    if initial.activity.edition == 5 && reader.options.identity != IdentityDetail::AzureDatabase {
        read_context(reader, IdentityDetail::AzureDatabase).await
    } else {
        Ok(initial)
    }
}

pub(super) async fn read<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    initial: &LookupContext,
) -> Result<LookupContext> {
    let identity = if initial.activity.edition == 5 { IdentityDetail::AzureDatabase } else { IdentityDetail::Endpoint };
    read_context(reader, identity).await
}

async fn read_context<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    identity: IdentityDetail,
) -> Result<LookupContext> {
    let native = reader
        .io
        .query(&super::sql::context(identity), vec![Parameter::Integer(1)], 1)
        .await
        .map_err(ReadError::into_error)?;
    let [row] = native.as_slice() else { return Err(invalid_data()) };
    let activity = ActivityContext::decode(row, &reader.host, reader.port, identity)?;
    let supported = match activity.edition {
        5 => true,
        2 | 3 | 4 | 8 => activity.major >= 15,
        _ => false,
    };
    let mut unavailable = if !supported || !boolean(row, "page_info_available")? {
        Some(PageWaitResolution::Unsupported)
    } else if !boolean(row, "page_permission")? {
        Some(PageWaitResolution::PermissionDenied)
    } else {
        None
    };
    let database_id = u32::try_from(integer(row, "selected_database_id")?).map_err(|_| invalid_data())?;
    if !(1..=i16::MAX as u32).contains(&database_id) {
        return Err(invalid_data());
    }
    let database_created = crate::sql_time::native_time(&text(row, "database_created_without_offset")?)?;
    let physical_database =
        if identity == IdentityDetail::AzureDatabase { optional_text(row, "physical_incarnation")? } else { None };
    let instance = if activity.edition == 5 || unavailable.is_some() {
        None
    } else {
        match instance::read(&mut reader.io).await {
            Ok((context, _)) => Some(context),
            Err(error) if stops_remaining_reads(&error) => return Err(error),
            Err(error) => {
                unavailable = Some(super::error_resolution(error.code));
                None
            }
        }
    };
    Ok(LookupContext {
        activity,
        database_id,
        database_created,
        physical_database,
        instance,
        unavailable,
        database_file_guid: optional_text(row, "database_file_guid")?,
        database_file_create_lsn: optional_text(row, "database_file_create_lsn")?,
    })
}
