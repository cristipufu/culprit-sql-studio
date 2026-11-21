//! The logical database qualifies Query Store IDs; physical incarnation and role only guard this acquisition.
use crate::{
    read::{NativeRead, ReadError, database_writable, integer, invalid_data, optional_text, text},
    value::Rec,
};
use culprit_core::{DatabaseIdentity, Engine, Result, telemetry::CollectionStop};

pub(crate) mod catalog;
pub(crate) mod log_rates;

const AZURE_COLUMNS: &str = "database_name,CONVERT(varchar(36),logical_database_guid) AS incarnation,CONVERT(varchar(36),physical_database_guid) AS physical_incarnation,CAST(DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS nvarchar(32)) AS updateability";
const AZURE_RELATION: &str = "sys.dm_user_db_resource_governance WHERE database_id=DB_ID() AND database_name=DB_NAME()";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    pub database: String,
    pub incarnation: Option<String>,
    physical_incarnation: Option<String>,
    pub writable: Option<bool>,
    pub edition: i64,
}

impl Context {
    fn decode(row: &Rec, edition: i64) -> Result<Self> {
        let value = Self {
            database: text(row, "database_name")?,
            incarnation: optional_text(row, "incarnation")?,
            physical_incarnation: optional_text(row, "physical_incarnation")?,
            writable: database_writable(row)?,
            edition,
        };
        if value.database.trim().is_empty()
            || value.incarnation.is_some() != value.physical_incarnation.is_some()
            || value.incarnation.as_deref().is_some_and(str::is_empty)
            || value.physical_incarnation.as_deref().is_some_and(str::is_empty)
            || edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE && value.incarnation.is_some()
        {
            return Err(invalid_data());
        }
        Ok(value)
    }

    pub(crate) fn from_governance(row: &Rec) -> Result<Self> {
        let context = Self::decode(row, crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE)?;
        if context.incarnation.is_none() {
            return Err(invalid_data());
        }
        Ok(context)
    }

    pub(crate) fn from_window(row: &Rec) -> Result<Self> {
        let edition = integer(row, "engine_edition")?;
        let rows = integer(row, "native_identity_rows")?;
        if rows > 1 {
            return Err(CollectionStop::SourceChanged.error());
        }
        let context = Self::decode(row, edition)?;
        if rows == 0 && context.incarnation.is_some()
            || edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE && rows != 0
        {
            return Err(invalid_data());
        }
        Ok(context)
    }

    pub(crate) fn identity(&self, host: &str, port: u16) -> DatabaseIdentity {
        let mut identity = DatabaseIdentity::server(Engine::Mssql, &self.database, host, port, None);
        identity.incarnation = self.incarnation.clone();
        identity
    }

    pub(crate) fn physical_incarnation(&self) -> Option<&str> {
        self.physical_incarnation.as_deref()
    }
}

/// Existing collector identity admission stays strict even though ordinary history can retain unknown identity.
#[derive(Debug)]
pub(crate) struct GovernanceRead {
    pub context: Context,
    pub configured_log_rates: Result<Option<culprit_core::telemetry::ConfiguredLogRateLimits>>,
}

pub(crate) async fn read_azure(reader: &mut impl NativeRead) -> Result<Context> {
    Ok(read_azure_detail(reader, log_rates::Detail::Identity).await?.context)
}

pub(crate) async fn read_azure_detail(
    reader: &mut impl NativeRead,
    detail: log_rates::Detail,
) -> Result<GovernanceRead> {
    let rows = reader.query(&azure_query_detail(detail), vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let context = Context::from_governance(row)?;
    let configured_log_rates = if detail == log_rates::Detail::ConfiguredLogRates {
        log_rates::decode(row, &context, "configured_log_observed_at")
    } else {
        Ok(None)
    };
    Ok(GovernanceRead { context, configured_log_rates })
}

fn azure_query_detail(detail: log_rates::Detail) -> String {
    match detail {
        log_rates::Detail::Identity => azure_query(),
        log_rates::Detail::ConfiguredLogRates => format!(
            "SELECT /* culprit */ {AZURE_COLUMNS},{},{} FROM {AZURE_RELATION} OPTION(MAXDOP 1)",
            log_rates::COLUMNS,
            log_rates::CLOCK
        ),
    }
}

fn azure_query() -> String {
    format!("SELECT /* culprit */ {AZURE_COLUMNS} FROM {AZURE_RELATION} OPTION(MAXDOP 1)")
}

/// The optional identity lookup stays inside the existing window batch. Azure permission gaps cannot erase an
/// otherwise readable Query Store population. No other platform parses an Azure-only catalog reference.
pub(crate) fn window_declaration(edition: i64) -> String {
    window_declaration_detail(edition, log_rates::Detail::Identity)
}

pub(crate) fn window_declaration_detail(edition: i64, detail: log_rates::Detail) -> String {
    if edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
        return String::new();
    }
    let (declarations, assignments, fields) = match detail {
        log_rates::Detail::Identity => ("", "", String::new()),
        log_rates::Detail::ConfiguredLogRates => {
            (log_rates::declaration(), log_rates::assignment(), format!(",{}", log_rates::COLUMNS))
        }
    };
    // Both native IDs and the optional configuration come from the same selected row. A second row remains
    // an ambiguity witness; an independent limit change is not part of database-incarnation equality.
    format!(
        r#"DECLARE /* culprit */ @culprit_identity_rows bigint=0,
 @culprit_logical_database_guid varchar(36)=NULL,@culprit_physical_database_guid varchar(36)=NULL{declarations};
BEGIN TRY
 SELECT @culprit_identity_rows=COUNT_BIG(*),@culprit_logical_database_guid=MIN(native.incarnation),
  @culprit_physical_database_guid=MIN(native.physical_incarnation){assignments}
 FROM (SELECT TOP(2) {AZURE_COLUMNS}{fields} FROM {AZURE_RELATION}) native OPTION(MAXDOP 1);
END TRY
BEGIN CATCH
 IF ERROR_NUMBER() NOT IN(229,297,300) THROW;
 SET @culprit_identity_rows=0;
 SET @culprit_logical_database_guid=NULL;
 SET @culprit_physical_database_guid=NULL;
END CATCH;
"#
    )
}

pub(crate) fn window_columns_detail(edition: i64, detail: log_rates::Detail) -> String {
    let mut columns = window_columns(edition).to_owned();
    if edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE && detail == log_rates::Detail::ConfiguredLogRates {
        columns.push_str(log_rates::columns());
    }
    columns
}

pub(crate) fn window_columns(edition: i64) -> &'static str {
    if edition == crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
        "DB_NAME() AS database_name,CAST(SERVERPROPERTY('EngineEdition') AS int) AS engine_edition,CAST(DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS nvarchar(32)) AS updateability,@culprit_identity_rows AS native_identity_rows,@culprit_logical_database_guid AS incarnation,@culprit_physical_database_guid AS physical_incarnation"
    } else {
        "DB_NAME() AS database_name,CAST(SERVERPROPERTY('EngineEdition') AS int) AS engine_edition,CAST(DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS nvarchar(32)) AS updateability,CAST(0 AS bigint) AS native_identity_rows,CAST(NULL AS varchar(36)) AS incarnation,CAST(NULL AS varchar(36)) AS physical_incarnation"
    }
}

#[cfg(test)]
mod tests;
