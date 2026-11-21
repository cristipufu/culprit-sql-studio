//! Source-local database authority. Recovery/file GUIDs and native process epochs are never converted to UTC
//! or presented as global host identities. Page availability is an independent diagnostic capability.
use crate::activity::{ActivityContext, IdentityDetail, sql};
use crate::{
    instance,
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_text, stops_remaining_reads, text},
    value::Rec,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
mod process;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Context {
    pub activity: ActivityContext,
    pub database_id: u32,
    pub catalog_database_id: u32,
    pub database_created: String,
    pub physical_database: Option<String>,
    pub database_file_guid: Option<String>,
    pub database_file_create_lsn: Option<String>,
    pub database_guid: Option<String>,
    pub recovery_fork_guid: Option<String>,
    pub instance: Option<instance::Context>,
    pub database_wide: bool,
    pub qualification_error: Option<ErrorCode>,
    process: Option<process::ProcessEpoch>,
    connection_generation: Option<u64>,
}
impl Context {
    /// Observe source authority through the caller's existing bounded native reader.
    pub(crate) async fn read<R: NativeRead>(reader: &mut R, host: &str, port: u16) -> Result<Self> {
        read_initial(reader, host, port, IdentityDetail::Endpoint, Detail::Authority).await.map(|(context, _)| context)
    }
    pub(crate) async fn recheck<R: NativeRead>(&self, reader: &mut R, host: &str, port: u16) -> Result<Self> {
        read_again(reader, host, port, self, Detail::Authority).await.map(|(context, _)| context)
    }
    /// These bounds describe acquisition, not the lifetime or execution origin of retained native values.
    pub(crate) fn completed(&self, after: &Self) -> Result<NativeReadContext> {
        if !self.same_observed_source(after) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let context = NativeReadContext {
            target: self.activity.database.clone().into(),
            partition: self.source_partition()?,
            observed_from: self.activity.at.clone(),
            observed_until: after.activity.at.clone(),
            database_writable: self.activity.writable,
        };
        if !context.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(context)
    }
    pub(crate) fn matches_frame(&self, frame: &ActivityContext) -> bool {
        self.activity.edition == frame.edition
            && self.activity.major == frame.major
            && self.activity.database.database == frame.database.database
            && self.activity.writable == frame.writable
            && self.activity.replica_id == frame.replica_id
            && self.activity.request_page_server_read_capability == frame.request_page_server_read_capability
            && frame.epoch.as_ref().is_none_or(|epoch| self.activity.epoch.as_ref() == Some(epoch))
    }
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.is_qualified() && other.is_qualified() && self.same_observed_source(other)
    }
    pub(crate) fn same_observed_source(&self, other: &Self) -> bool {
        self.activity.same_source(&other.activity)
            && self.database_id == other.database_id
            && self.catalog_database_id == other.catalog_database_id
            && self.database_created == other.database_created
            && self.physical_database == other.physical_database
            && self.database_file_guid == other.database_file_guid
            && self.database_file_create_lsn == other.database_file_create_lsn
            && self.database_guid == other.database_guid
            && self.recovery_fork_guid == other.recovery_fork_guid
            && self.instance == other.instance
            && self.process == other.process
            && self.connection_generation == other.connection_generation
    }
    pub(crate) fn is_qualified(&self) -> bool {
        self.qualification_error.is_none() && self.activity.writable.is_some() && self.native_identity().is_ok()
    }
    pub(crate) fn native_identity(&self) -> Result<NativeIdentity> {
        let identity = if self.activity.edition == 5 {
            NativeIdentity::new(self.activity.database.incarnation.as_ref().ok_or_else(invalid_data)?)
                .qualified("kind", "azure-sql-logical-database")
                .qualified("physical_database_guid", self.physical_database.as_ref().ok_or_else(invalid_data)?)
                .qualified("replica_id", self.activity.replica_id.as_ref().ok_or_else(invalid_data)?)
                .qualified("process_epoch", self.process.as_ref().ok_or_else(invalid_data)?.key()?)
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
        if self.is_qualified() {
            // database_guid can be duplicated by copying/attaching a database. Preserve it in the local
            // authority token; DatabaseIdentity::incarnation would instead promote it to a global namespace.
            let identity = self.native_identity()?;
            context.epoch = Some(identity.key()?);
        } else {
            context.epoch = None;
        }
        Ok(context)
    }

    pub(crate) fn source_instance(&self) -> Option<SharedResourceIdentity> {
        if self.activity.edition != 5 {
            return self.instance.as_ref().map(|instance| instance.identity.clone());
        }
        let identity = SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: SharedResourceKind::Node,
            namespace: NativeIdentity::new(self.activity.database.incarnation.as_ref()?)
                .qualified("kind", "azure-sql-logical-database"),
            native: NativeIdentity::new(self.activity.replica_id.as_ref()?)
                .qualified("kind", "azure-sql-hyperscale-replica"),
        };
        identity.is_valid().then_some(identity)
    }

    pub(crate) fn source_partition(&self) -> Result<SourcePartition> {
        Ok(SourcePartition::InstanceLocal { member: self.source_instance(), epoch: self.capture_context()?.epoch })
    }

    pub(crate) fn connection_changed(&self, other: &Self) -> bool {
        self.connection_generation != other.connection_generation
    }

    pub(crate) fn connection_generation(&self) -> Option<u64> {
        self.connection_generation
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Detail {
    Authority,
    /// Activity request counters need their native column capability; unrelated authority reads do not.
    Activity,
    PageLookup,
    /// Native task-space permissions share the same source authority point.
    TaskSpace,
}

pub(crate) async fn read_initial<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    identity: IdentityDetail,
    detail: Detail,
) -> Result<(Context, Rec)> {
    reader.observe_session_work(crate::read::session_work::Observation::Started);
    let result = read_initial_context(reader, host, port, identity, detail).await;
    observe_context_result(reader, result)
}

async fn read_initial_context<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    identity: IdentityDetail,
    detail: Detail,
) -> Result<(Context, Rec)> {
    if identity == IdentityDetail::AzureDatabase {
        return read_projection(reader, Address { host, port }, identity, None, None, detail).await;
    }
    let platform = instance::observe(reader).await?;
    let connection_generation = reader.connection_generation();
    if platform.edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE && !platform.supports_instance() {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let (instance, error) = match platform.context(reader).await {
        Ok(context) => (context, None),
        Err(error) if stops_remaining_reads(&error) => return Err(error),
        Err(error) => (None, Some(error.code)),
    };
    let identity = IdentityDetail::for_native_edition(platform.edition);
    let mut result = read_projection(reader, Address { host, port }, identity, instance, error, detail).await?;
    if connection_generation != reader.connection_generation() {
        result.0.instance = None;
        result.0.qualification_error = Some(ErrorCode::ConnectionFailed);
    }
    if result.0.activity.edition != platform.edition
        || result.0.activity.major != platform.major
        || platform.observed_at > result.0.activity.at
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(result)
}
pub(crate) async fn read_again<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    initial: &Context,
    detail: Detail,
) -> Result<(Context, Rec)> {
    reader.observe_session_work(crate::read::session_work::Observation::Started);
    let result = match IdentityDetail::for_native_edition(initial.activity.edition) {
        IdentityDetail::AzureDatabase => {
            read_projection(reader, Address { host, port }, IdentityDetail::AzureDatabase, None, None, detail).await
        }
        IdentityDetail::Endpoint => read_initial_context(reader, host, port, IdentityDetail::Endpoint, detail).await,
    };
    observe_context_result(reader, result)
}
fn observe_context_result<R: NativeRead>(reader: &mut R, result: Result<(Context, Rec)>) -> Result<(Context, Rec)> {
    match &result {
        Ok((context, row)) => crate::read::session_work::observe(reader, context, row),
        Err(_) => reader.observe_session_work(crate::read::session_work::Observation::Unavailable(
            NativeReadWorkIssue::NativeScopeUnqualified,
        )),
    }
    result
}
struct Address<'a> {
    host: &'a str,
    port: u16,
}
async fn read_projection<R: NativeRead>(
    reader: &mut R,
    address: Address<'_>,
    identity: IdentityDetail,
    instance: Option<instance::Context>,
    qualification_error: Option<ErrorCode>,
    detail: Detail,
) -> Result<(Context, Rec)> {
    let rows =
        reader.query(&query(identity, detail), vec![Parameter::Integer(1)], 1).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        return Err(invalid_data());
    };
    let mut context = decode(row, address.host, address.port, identity, instance, qualification_error)?;
    context.connection_generation = reader.connection_generation();
    Ok((context, rows.into_iter().next().ok_or_else(invalid_data)?))
}

fn decode(
    row: &Rec,
    host: &str,
    port: u16,
    identity: IdentityDetail,
    instance: Option<instance::Context>,
    qualification_error: Option<ErrorCode>,
) -> Result<Context> {
    let (activity, database_wide) = ActivityContext::observed(row, host, port, identity)?;
    let database_id = u32::try_from(integer(row, "selected_database_id")?).map_err(|_| invalid_data())?;
    if !(1..=i16::MAX as u32).contains(&database_id) {
        return Err(invalid_data());
    }
    let catalog_database_id = u32::try_from(integer(row, "catalog_database_id")?).map_err(|_| invalid_data())?;
    if catalog_database_id == 0
        || i32::try_from(catalog_database_id).is_err()
        || text(row, "catalog_database_name")? != activity.database.database
        || (activity.edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE && catalog_database_id != database_id)
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    let process = process::ProcessEpoch::decode(row)?;
    let context = Context {
        activity,
        database_id,
        catalog_database_id,
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
        process,
        connection_generation: None,
    };
    Ok(context)
}

#[cfg(test)]
pub(crate) fn test_azure_authority(row: &Rec) -> Result<Context> {
    decode(&crate::activity::tests::authority(row), "fixture.invalid", 1433, IdentityDetail::AzureDatabase, None, None)
}

fn query(identity: IdentityDetail, detail: Detail) -> String {
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
        Detail::Authority | Detail::Activity => String::new(),
        Detail::TaskSpace => format!(",{} AS task_space_permission", crate::activity::tempdb::TASK_SPACE_PERMISSION),
        Detail::PageLookup => {
            ",CASE WHEN OBJECT_ID('sys.dm_db_page_info') IS NOT NULL THEN 1 ELSE 0 END AS page_info_available,CASE WHEN HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE')=1 OR HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE PERFORMANCE STATE')=1 THEN 1 ELSE 0 END AS page_permission".to_owned()
        }
    };
    let rows = format!(
        r#"SELECT CAST(NULL AS int) AS session_id,
 {current_database_id} AS selected_database_id,sd.database_id AS catalog_database_id,sd.name AS catalog_database_name,CONVERT(varchar(40),CAST(sd.create_date AS datetime2),126) AS database_created_without_offset,
 CONVERT(varchar(36),df.file_guid) AS database_file_guid,CONVERT(varchar(30),df.create_lsn) AS database_file_create_lsn,
 {recovery},{process_columns}{page},
 collector.session_id AS collector_session_id,
 CONVERT(varchar(40),CAST(collector.login_time AS datetime2),126) AS collector_login_without_offset,
 collector.cpu_time AS collector_cpu_ms,collector.reads AS collector_reads,
 collector.logical_reads AS collector_logical_reads,collector.writes AS collector_dirty_marks,
 collector.memory_usage AS collector_memory_pages
 FROM sys.databases sd LEFT JOIN sys.database_files df ON df.file_id=1 AND df.type=0
 LEFT JOIN sys.dm_exec_sessions collector ON collector.session_id=@@SPID
 {join} WHERE {catalog_predicate}"#,
        current_database_id = crate::database_identity::catalog::CURRENT_DATABASE_ID,
        catalog_predicate = crate::database_identity::catalog::predicate("sd"),
        process_columns =
            if identity == IdentityDetail::AzureDatabase { process::COLUMNS } else { process::UNAVAILABLE_COLUMNS }
    );
    let query = match detail {
        Detail::Authority => sql::frame_query(&rows, identity, sql::QueryPolicy::SerialMetadata),
        Detail::Activity | Detail::PageLookup | Detail::TaskSpace => {
            sql::request_frame_query(&rows, identity, sql::QueryPolicy::SerialMetadata)
        }
    };
    if identity == IdentityDetail::AzureDatabase { format!("{}{query}", process::AZURE_READ) } else { query }
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod tests;
