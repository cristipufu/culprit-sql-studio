//! Source-local database authority. Recovery/file GUIDs and native process epochs are never converted to UTC
//! or presented as global host identities. Page availability is an independent diagnostic capability.
use super::{ActivityContext, ActivityReader, IdentityDetail, sql};
use crate::{
    instance,
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_text, stops_remaining_reads, text},
    value::Rec,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DatabaseContext {
    pub activity: ActivityContext,
    pub database_id: u32,
    pub database_created: String,
    pub physical_database: Option<String>,
    pub database_file_guid: Option<String>,
    pub database_file_create_lsn: Option<String>,
    pub database_guid: Option<String>,
    pub recovery_fork_guid: Option<String>,
    pub instance: Option<instance::Context>,
    pub database_wide: bool,
    pub qualification_error: Option<ErrorCode>,
}
impl DatabaseContext {
    pub(crate) async fn initial<R: NativeRead>(reader: &mut ActivityReader<R>) -> Result<Self> {
        read_initial(reader, ContextDetail::Authority).await.map(|(context, _)| context)
    }
    pub(crate) async fn recheck<R: NativeRead>(reader: &mut ActivityReader<R>, initial: &Self) -> Result<Self> {
        read_again(reader, initial, ContextDetail::Authority).await.map(|(context, _)| context)
    }
    pub(crate) fn matches_frame(&self, frame: &ActivityContext) -> bool {
        self.activity.edition == frame.edition
            && self.activity.major == frame.major
            && self.activity.database.database == frame.database.database
            && self.activity.writable == frame.writable
            && frame.epoch.as_ref().is_none_or(|epoch| self.activity.epoch.as_ref() == Some(epoch))
    }
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.is_qualified() && other.is_qualified() && self.same_observed_source(other)
    }
    pub(crate) fn same_observed_source(&self, other: &Self) -> bool {
        self.activity.same_source(&other.activity)
            && self.database_id == other.database_id
            && self.database_created == other.database_created
            && self.physical_database == other.physical_database
            && self.database_file_guid == other.database_file_guid
            && self.database_file_create_lsn == other.database_file_create_lsn
            && self.database_guid == other.database_guid
            && self.recovery_fork_guid == other.recovery_fork_guid
            && self.instance == other.instance
    }
    pub(crate) fn is_qualified(&self) -> bool {
        self.native_identity().is_ok()
    }
    pub(crate) fn native_identity(&self) -> Result<NativeIdentity> {
        let identity = if self.activity.edition == 5 {
            NativeIdentity::new(self.activity.database.incarnation.as_ref().ok_or_else(invalid_data)?)
                .qualified("kind", "azure-sql-logical-database")
                .qualified("physical_database_guid", self.physical_database.as_ref().ok_or_else(invalid_data)?)
        } else {
            let mut identity = NativeIdentity::new(self.database_id.to_string())
                .qualified("kind", "sql-server-local-database-recovery-epoch")
                .qualified("database_created_without_offset", &self.database_created)
                .qualified("database_file_guid", self.database_file_guid.as_ref().ok_or_else(invalid_data)?)
                .qualified("database_guid", self.database_guid.as_ref().ok_or_else(invalid_data)?)
                .qualified("recovery_fork_guid", self.recovery_fork_guid.as_ref().ok_or_else(invalid_data)?)
                .qualified("instance_epoch", self.instance.as_ref().ok_or_else(invalid_data)?.capture_epoch()?);
            if let Some(lsn) = &self.database_file_create_lsn {
                identity = identity.qualified("database_file_create_lsn", lsn);
            }
            identity
        };
        if !identity.is_valid() {
            return Err(invalid_data());
        }
        Ok(identity)
    }
    pub(crate) fn capture_context(&self) -> Result<ActivityContext> {
        let mut context = self.activity.clone();
        if context.edition != 5 {
            // database_guid can be duplicated by copying/attaching a database. Preserve it in the local
            // authority token; DatabaseIdentity::incarnation would instead promote it to a global namespace.
            context.epoch = if self.is_qualified() {
                let identity = self.native_identity()?;
                Some(digest(&serde_json::to_vec(&identity).map_err(|_| invalid_data())?))
            } else {
                None
            };
        } else if !self.is_qualified() {
            context.epoch = None;
        }
        Ok(context)
    }
}

#[derive(Clone, Copy)]
pub(super) enum ContextDetail {
    Authority,
    PageLookup,
}

pub(super) async fn read_initial<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    detail: ContextDetail,
) -> Result<(DatabaseContext, Rec)> {
    if reader.options.identity == IdentityDetail::AzureDatabase {
        return read(reader, IdentityDetail::AzureDatabase, None, None, detail).await;
    }
    let platform = instance::observe(&mut reader.io).await?;
    if platform.edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE && !platform.supports_instance() {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let (instance, error) = match platform.context(&mut reader.io).await {
        Ok(context) => (context, None),
        Err(error) if stops_remaining_reads(&error) => return Err(error),
        Err(error) => (None, Some(error.code)),
    };
    let identity = if platform.edition == 5 { IdentityDetail::AzureDatabase } else { IdentityDetail::Endpoint };
    let result = read(reader, identity, instance, error, detail).await?;
    if result.0.activity.edition != platform.edition
        || result.0.activity.major != platform.major
        || platform.observed_at > result.0.activity.at
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(result)
}
pub(super) async fn read_again<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    initial: &DatabaseContext,
    detail: ContextDetail,
) -> Result<(DatabaseContext, Rec)> {
    if initial.activity.edition == 5 {
        read(reader, IdentityDetail::AzureDatabase, None, None, detail).await
    } else {
        read_initial(reader, detail).await
    }
}
async fn read<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    identity: IdentityDetail,
    instance: Option<instance::Context>,
    qualification_error: Option<ErrorCode>,
    detail: ContextDetail,
) -> Result<(DatabaseContext, Rec)> {
    let rows = reader
        .io
        .query(&query(identity, detail), vec![Parameter::Integer(1)], 1)
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let (activity, database_wide) = ActivityContext::observed(row, &reader.host, reader.port, identity)?;
    let database_id = u32::try_from(integer(row, "selected_database_id")?).map_err(|_| invalid_data())?;
    if !(1..=i16::MAX as u32).contains(&database_id) {
        return Err(invalid_data());
    }
    let context = DatabaseContext {
        activity,
        database_id,
        database_created: crate::sql_time::native_time(&text(row, "database_created_without_offset")?)?,
        physical_database: if identity == IdentityDetail::AzureDatabase {
            optional_text(row, "physical_incarnation")?
        } else {
            None
        },
        database_file_guid: optional_text(row, "database_file_guid")?,
        database_file_create_lsn: optional_text(row, "database_file_create_lsn")?,
        database_guid: optional_text(row, "database_guid")?,
        recovery_fork_guid: optional_text(row, "recovery_fork_guid")?,
        instance,
        database_wide,
        qualification_error,
    };
    Ok((context, rows.into_iter().next().ok_or_else(invalid_data)?))
}
fn query(identity: IdentityDetail, detail: ContextDetail) -> String {
    // recovery_status is documented only for boxed SQL Server/MI. Native product selection happens before
    // this SQL is constructed; an Azure database never parses an unsupported catalog reference.
    let (recovery, join) = if identity == IdentityDetail::AzureDatabase {
        ("CAST(NULL AS varchar(36)) AS database_guid,CAST(NULL AS varchar(36)) AS recovery_fork_guid", "")
    } else {
        (
            "CONVERT(varchar(36),recovery.database_guid) AS database_guid,CONVERT(varchar(36),recovery.recovery_fork_guid) AS recovery_fork_guid",
            "LEFT JOIN sys.database_recovery_status recovery ON recovery.database_id=sd.database_id",
        )
    };
    let page = match detail {
        ContextDetail::Authority => "",
        ContextDetail::PageLookup => {
            ",CASE WHEN OBJECT_ID('sys.dm_db_page_info') IS NOT NULL THEN 1 ELSE 0 END AS page_info_available,CASE WHEN HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE')=1 OR HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE PERFORMANCE STATE')=1 THEN 1 ELSE 0 END AS page_permission"
        }
    };
    let rows = format!(
        r#"SELECT CAST(NULL AS int) AS session_id,
 sd.database_id AS selected_database_id,CONVERT(varchar(40),CAST(sd.create_date AS datetime2),126) AS database_created_without_offset,
 CONVERT(varchar(36),df.file_guid) AS database_file_guid,CONVERT(varchar(30),df.create_lsn) AS database_file_create_lsn,
 {recovery}{page}
 FROM sys.databases sd LEFT JOIN sys.database_files df ON df.file_id=1 AND df.type=0
 {join} WHERE sd.database_id=DB_ID()"#
    );
    sql::frame_query(&rows, identity, sql::QueryPolicy::SerialMetadata)
}

#[cfg(test)]
pub(crate) mod tests;
