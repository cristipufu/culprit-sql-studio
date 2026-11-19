//! A separately admitted shared target. The affected request stays in its application database; native DB2,
//! file and process facts qualify only the observed tempdb page. This profile remains cost-unqualified.
use super::{MAX_PAGES, PageAddress};
use crate::{
    local_source,
    read::{NativeRead, Parameter, boolean, integer, invalid_data, optional_text, stops_remaining_reads, text},
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use std::collections::BTreeMap;

const TEMPDB_DATABASE_ID: u32 = 2;

#[derive(Clone, PartialEq, Eq)]
struct Database {
    created: String,
    guid: String,
    recovery_fork: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Context {
    resource: SharedResourceIdentity,
    epoch: String,
    database: Option<Database>,
    files: BTreeMap<u32, super::file_epoch::ObservedFile>,
    permission: bool,
    unavailable: Option<PageWaitResolution>,
    observed_from: String,
    observed_until: String,
}

impl Context {
    pub(super) async fn initial<R: NativeRead>(
        reader: &mut R,
        authority: &local_source::Context,
        expected: &SharedResourceIdentity,
        page_info_available: bool,
    ) -> Result<Self> {
        // No Azure tempdb/instance identity is inferred from a database route, role or database_id=2.
        let instance = authority.instance.as_ref().ok_or_else(unsupported)?;
        if !authority.is_qualified()
            || !super::context::supports_page_info(
                authority.activity.edition,
                authority.activity.major,
                page_info_available,
            )
        {
            return Err(unsupported());
        }
        if instance.identity != *expected || authority.source_instance().as_ref() != Some(expected) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let epoch = instance.capture_epoch()?;
        let mut context = Self {
            resource: instance.identity.clone(),
            epoch,
            database: None,
            files: BTreeMap::new(),
            permission: false,
            unavailable: None,
            observed_from: authority.activity.at.clone(),
            observed_until: authority.activity.at.clone(),
        };
        context.read(reader, authority).await?;
        Ok(context)
    }

    pub(super) async fn recheck<R: NativeRead>(
        &self,
        reader: &mut R,
        authority: &local_source::Context,
    ) -> Result<Self> {
        let instance = authority.instance.as_ref().ok_or_else(|| CollectionStop::SourceChanged.error())?;
        if instance.identity != self.resource || instance.capture_epoch()? != self.epoch {
            return Err(CollectionStop::SourceChanged.error());
        }
        let mut checked = self.clone();
        checked.read(reader, authority).await?;
        Ok(checked)
    }

    async fn read<R: NativeRead>(&mut self, reader: &mut R, authority: &local_source::Context) -> Result<()> {
        let mut files = String::from("<files>");
        for file in self.files.keys() {
            files.push_str(&format!("<file id=\"{file}\"/>"));
        }
        files.push_str("</files>");
        let rows = match reader.query(&metadata_sql(), vec![Parameter::Text(files)], self.files.len().max(1)).await {
            Ok(rows) => rows,
            Err(error) => {
                let error = error.into_error();
                if stops_remaining_reads(&error) || error.detail.is_some() {
                    return Err(error);
                }
                let reason = match error.code {
                    ErrorCode::PermissionDenied => PageWaitResolution::PermissionDenied,
                    ErrorCode::Unsupported => PageWaitResolution::Unsupported,
                    _ => return Err(error),
                };
                self.database = None;
                self.permission = false;
                self.unavailable = Some(reason);
                self.observed_until = authority.activity.at.clone();
                return Ok(());
            }
        };
        if rows.len() != self.files.len().max(1) {
            return Err(invalid_data());
        }
        let first = rows.first().ok_or_else(invalid_data)?;
        let observed_at = canonical_time(&text(first, "observed_at")?)?;
        if self.observed_from > observed_at || self.observed_until > observed_at || authority.activity.at > observed_at
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let permission = boolean(first, "tempdb_page_permission")?;
        let decode_database = |row: &crate::value::Rec| -> Result<Option<Database>> {
            if integer(row, "tempdb_database_id")? != i64::from(TEMPDB_DATABASE_ID)
                || text(row, "tempdb_name")? != "tempdb"
            {
                return Err(invalid_data());
            }
            let created = optional_text(row, "tempdb_created_without_offset")?
                .map(|time| crate::sql_time::native_time(&time))
                .transpose()?;
            let guid = optional_text(row, "tempdb_database_guid")?;
            let recovery_fork = optional_text(row, "tempdb_recovery_fork_guid")?;
            Ok(created.zip(guid).zip(recovery_fork).map(|((created, guid), recovery_fork)| Database {
                created,
                guid,
                recovery_fork,
            }))
        };
        let database = decode_database(first)?;
        let mut files = BTreeMap::new();
        for row in &rows {
            if canonical_time(&text(row, "observed_at")?)? != observed_at
                || decode_database(row)? != database
                || boolean(row, "tempdb_page_permission")? != permission
            {
                return Err(CollectionStop::SourceChanged.error());
            }
            let selected = crate::read::optional_integer(row, "selected_file_id")?;
            if self.files.is_empty() {
                if selected.is_some() {
                    return Err(invalid_data());
                }
                continue;
            }
            let selected: u32 = selected.ok_or_else(invalid_data)?.try_into().map_err(|_| invalid_data())?;
            if !self.files.contains_key(&selected)
                || files.insert(selected, super::file_epoch::ObservedFile::decode(row)?).is_some()
            {
                return Err(invalid_data());
            }
        }
        self.database = database;
        self.files = files;
        self.permission = permission;
        self.unavailable = (!permission)
            .then_some(PageWaitResolution::PermissionDenied)
            .or_else(|| self.database.is_none().then_some(PageWaitResolution::UnverifiedSource));
        self.observed_until = observed_at;
        Ok(())
    }

    pub(super) fn same_source(&self, other: &Self) -> bool {
        self.resource == other.resource
            && self.epoch == other.epoch
            && self.database == other.database
            && self.files == other.files
            && self.permission == other.permission
            && self.unavailable == other.unavailable
    }

    pub(super) fn unavailable(&self) -> Option<PageWaitResolution> {
        self.unavailable
    }

    pub(super) fn observed_until(&self) -> &str {
        &self.observed_until
    }

    pub(super) fn target(&self) -> CollectionTarget {
        CollectionTarget::Shared(self.resource.clone())
    }

    pub(super) fn partition(&self) -> SourcePartition {
        SourcePartition::InstanceLocal { member: Some(self.resource.clone()), epoch: Some(self.epoch.clone()) }
    }

    pub(super) fn read_context(&self) -> Result<NativeReadContext> {
        if self.unavailable.is_some() {
            return Err(invalid_data());
        }
        let context = NativeReadContext {
            target: self.target(),
            partition: self.partition(),
            observed_from: self.observed_from.clone(),
            observed_until: self.observed_until.clone(),
            database_writable: None,
        };
        if context.has_qualified_local_owner() { Ok(context) } else { Err(invalid_data()) }
    }

    pub(super) fn database_identity(&self) -> Result<NativeIdentity> {
        let database = self.database.as_ref().ok_or_else(invalid_data)?;
        let identity = NativeIdentity::new(TEMPDB_DATABASE_ID.to_string())
            .qualified("kind", MSSQL_SHARED_TEMPDB_DATABASE_KIND)
            .qualified("database_created_without_offset", &database.created)
            .qualified("database_guid", &database.guid)
            .qualified("recovery_fork_guid", &database.recovery_fork)
            .qualified("instance_epoch", &self.epoch);
        if identity.is_valid() { Ok(identity) } else { Err(invalid_data()) }
    }

    pub(super) fn retain_file(&mut self, epoch: &super::file_epoch::FileEpoch) -> Result<()> {
        let file = epoch.observed_file();
        let file_id = epoch.file_id();
        if (!self.files.contains_key(&file_id) && self.files.len() == MAX_PAGES)
            || self.files.get(&file_id).is_some_and(|previous| previous != &file)
        {
            return Err(invalid_data());
        }
        self.files.insert(file_id, file);
        Ok(())
    }

    pub(super) fn accepts(address: PageAddress) -> bool {
        address.database == TEMPDB_DATABASE_ID
    }
}

fn metadata_sql() -> String {
    // Exact known tempdb row and only files selected from observed mapped pages; no file/page enumeration.
    // VIEW DATABASE PERFORMANCE STATE applies on SQL Server2022+, otherwise VIEW DATABASE STATE.
    // https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-views/sys-dm-db-page-info-transact-sql
    String::from(
        r#"DECLARE /* culprit */ @files xml=CAST(@P1 AS xml),@at datetime2=SYSUTCDATETIME();
SELECT /* culprit */ CONVERT(varchar(40),@at,126)+'Z' AS observed_at,
 d.database_id AS tempdb_database_id,d.name AS tempdb_name,
 CONVERT(varchar(40),CAST(d.create_date AS datetime2),126) AS tempdb_created_without_offset,
 CONVERT(varchar(36),rs.database_guid) AS tempdb_database_guid,
 CONVERT(varchar(36),rs.recovery_fork_guid) AS tempdb_recovery_fork_guid,
 CASE WHEN CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int)>=16
  THEN CASE WHEN HAS_PERMS_BY_NAME('tempdb','DATABASE','VIEW DATABASE PERFORMANCE STATE')=1 THEN 1 ELSE 0 END
  ELSE CASE WHEN HAS_PERMS_BY_NAME('tempdb','DATABASE','VIEW DATABASE STATE')=1 THEN 1 ELSE 0 END END AS tempdb_page_permission,
 selected.file_id AS selected_file_id,CONVERT(varchar(36),f.file_guid) AS file_guid,
 CONVERT(varchar(30),f.create_lsn) AS file_create_lsn,f.type AS file_type,f.state AS file_state
FROM sys.databases d
LEFT JOIN sys.database_recovery_status rs ON rs.database_id=d.database_id
OUTER APPLY(SELECT n.value('@id[1]','int') AS file_id FROM @files.nodes('/files/file') chosen(n)) selected
LEFT JOIN tempdb.sys.database_files f ON f.file_id=selected.file_id
WHERE d.database_id=DB_ID(N'tempdb') AND d.database_id=2 AND d.name=N'tempdb'
OPTION(MAXDOP 1,LOOP JOIN)"#,
    )
}

fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))
}

#[cfg(test)]
pub(super) mod tests;
