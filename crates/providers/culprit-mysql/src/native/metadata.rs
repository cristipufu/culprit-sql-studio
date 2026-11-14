use super::*;
use crate::connect::{Connector, Flavor};
use culprit_core::{DatabaseIdentity, Engine};
use mysql_async::Value;

#[derive(Clone, Debug)]
pub(crate) struct ConnectionContext {
    pub database: DatabaseIdentity,
    pub database_name: String,
    pub server_id: u64,
    pub server_name: String,
    pub connection_id: u64,
    pub version: String,
    pub flavor: Flavor,
    pub login: String,
    pub at: String,
    pub performance_schema: bool,
    pub read_only: bool,
}
impl ConnectionContext {
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.database == other.database
            && self.database_name == other.database_name
            && self.server_id == other.server_id
            && self.server_name == other.server_name
            && self.connection_id == other.connection_id
            && self.version == other.version
            && self.flavor == other.flavor
            && self.login == other.login
            && self.performance_schema == other.performance_schema
            && self.read_only == other.read_only
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Metadata {
    pub connection: ConnectionContext,
    pub database_wide: bool,
    pub process_wide: bool,
    /// Server process lifetime only. Never a reset marker for independently reset digest/table/status counters.
    pub started_second: Option<u64>,
}
impl Metadata {
    pub(crate) fn same_activity_source(&self, other: &Self) -> bool {
        self.connection.same_source(&other.connection) && self.started_second == other.started_second
    }
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.connection.same_source(&other.connection)
            && self.database_wide == other.database_wide
            && self.process_wide == other.process_wide
            && self.started_second == other.started_second
    }
}
pub(crate) async fn read<R: QueryRead>(reader: &mut R, connector: &Connector) -> Result<Metadata> {
    read_identity(reader, connector, false).await
}
pub(crate) async fn activity<R: QueryRead>(reader: &mut R, connector: &Connector) -> Result<Metadata> {
    read_identity(reader, connector, true).await
}
async fn read_identity<R: QueryRead>(reader: &mut R, connector: &Connector, process_epoch: bool) -> Result<Metadata> {
    let connection = connection_context(reader, connector).await?;
    let started_second = if process_epoch && connection.performance_schema {
        // The native process clock is independent of resets of digest/table/status counters.
        Some(process_started_second(reader, connection.flavor).await?)
    } else {
        None
    };
    let (user, host) = connection.login.rsplit_once('@').ok_or_else(invalid)?;
    let grantee = format!("'{}'@'{}'", user.replace('\'', "''"), host.replace('\'', "''"));
    // Direct global/schema privileges are sufficient proof. Role-only grants remain unverified rather than
    // interpreting server-specific role expansion or silently claiming an inventory is database-wide.
    let visibility = one(reader, "SELECT
        (EXISTS(SELECT 1 FROM information_schema.USER_PRIVILEGES WHERE GRANTEE=? AND PRIVILEGE_TYPE='SELECT')
          OR EXISTS(SELECT 1 FROM information_schema.SCHEMA_PRIVILEGES WHERE GRANTEE=? AND TABLE_SCHEMA=? AND PRIVILEGE_TYPE='SELECT')) AS database_wide,
        EXISTS(SELECT 1 FROM information_schema.USER_PRIVILEGES WHERE GRANTEE=? AND PRIVILEGE_TYPE='PROCESS') AS process_wide",
        vec![Value::from(grantee.clone()),Value::from(grantee.clone()),Value::from(connection.database_name.clone()),Value::from(grantee)]).await?;
    Ok(Metadata {
        connection,
        database_wide: flag(&visibility, "database_wide")?,
        process_wide: flag(&visibility, "process_wide")?,
        started_second,
    })
}

/// Native connection/database identity and its own UTC clock, without inventing unqueried visibility facts.
pub(crate) async fn connection_context<R: QueryRead>(
    reader: &mut R,
    connector: &Connector,
) -> Result<ConnectionContext> {
    let database = crate::probe::database_of(connector)?;
    let row = one(
        reader,
        "SELECT (SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME=?) AS database_name,
          @@GLOBAL.server_id AS server_id, @@hostname AS server_name, CONNECTION_ID() AS connection_id,
          VERSION() AS version, CURRENT_USER() AS login, @@performance_schema AS performance_schema,
          @@GLOBAL.read_only AS read_only,
          DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at",
        vec![Value::from(database.clone())],
    )
    .await?;
    let database_name = row.native_text("database_name")?.ok_or_else(|| {
        culprit_core::Error::not_found(culprit_core::t!("provider.mysql.probe.no_such_database", database = &database))
    })?;
    let version = text(&row, "version")?;
    let flavor = Flavor::of_version(&version);
    Ok(ConnectionContext {
        database: DatabaseIdentity::server(Engine::Mysql, &database_name, &connector.host, connector.port, None),
        database_name,
        server_id: integer(&row, "server_id")?,
        server_name: text(&row, "server_name")?,
        connection_id: integer(&row, "connection_id")?,
        flavor,
        version,
        login: text(&row, "login")?,
        at: text(&row, "observed_at")?,
        performance_schema: flag(&row, "performance_schema")?,
        read_only: flag(&row, "read_only")?,
    })
}

/// A native server-process marker, independently of any selected database or resettable statement counters.
/// Native Uptime is query-start seconds minus server start; reading both in one statement avoids clock skew.
/// https://github.com/mysql/mysql-server/blob/8.4/sql/mysqld.cc (show_starttime)
/// https://github.com/MariaDB/server/blob/11.8/sql/mysqld.cc (show_starttime)
pub(crate) async fn process_started_second<R: QueryRead>(reader: &mut R, flavor: Flavor) -> Result<u64> {
    let status = match flavor {
        Flavor::Mysql => "performance_schema.global_status",
        Flavor::Mariadb => "information_schema.GLOBAL_STATUS",
    };
    let clock = one(
        reader,
        &format!(
            "SELECT UNIX_TIMESTAMP() AS observed_second,
            CAST(VARIABLE_VALUE AS UNSIGNED) AS uptime FROM {status} WHERE VARIABLE_NAME='Uptime'"
        ),
        vec![],
    )
    .await?;
    integer(&clock, "observed_second")?.checked_sub(integer(&clock, "uptime")?).ok_or_else(invalid)
}
