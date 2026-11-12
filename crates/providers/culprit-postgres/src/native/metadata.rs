use super::*;
use culprit_core::{DatabaseIdentity, Engine};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StatementCatalog {
    pub schema: String,
    pub columns: BTreeSet<String>,
    pub info: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Metadata {
    pub database: DatabaseIdentity,
    pub database_oid: String,
    pub version: u32,
    pub started_at: String,
    pub at: String,
    pub block_bytes: u32,
    pub full_visibility: bool,
    pub standby: bool,
    pub statements: Option<StatementCatalog>,
}
impl Metadata {
    pub fn same_source(&self, other: &Self) -> bool {
        self.database == other.database
            && self.database_oid == other.database_oid
            && self.version == other.version
            && self.started_at == other.started_at
            && self.block_bytes == other.block_bytes
            && self.full_visibility == other.full_visibility
            && self.standby == other.standby
            && self.statements == other.statements
    }
}
pub(crate) async fn read<R: QueryRead>(reader: &mut R, host: &str, port: u16) -> Result<Metadata> {
    let sql = format!("SELECT pg_catalog.version() AS build, d.oid::text AS database_oid, d.datname AS database,
      pg_catalog.current_setting('server_version_num') AS version,
      pg_catalog.current_setting('block_size') AS block_bytes, pg_catalog.pg_is_in_recovery() AS standby, {} AS started_at, {} AS observed_at,
      (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user)
        OR pg_catalog.pg_has_role(current_user, 'pg_read_all_stats', 'MEMBER') AS full_visibility,
      (SELECT n.nspname FROM pg_catalog.pg_extension e JOIN pg_catalog.pg_namespace n ON n.oid=e.extnamespace WHERE e.extname='pg_stat_statements') AS pgss_schema,
      (SELECT pg_catalog.array_to_json(pg_catalog.array_agg(a.attname ORDER BY a.attnum))::text
         FROM pg_catalog.pg_extension e JOIN pg_catalog.pg_namespace n ON n.oid=e.extnamespace
         JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname='pg_stat_statements'
         JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped
        WHERE e.extname='pg_stat_statements') AS pgss_columns,
      EXISTS (SELECT 1 FROM pg_catalog.pg_extension e JOIN pg_catalog.pg_class c ON c.relnamespace=e.extnamespace
        JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_class'::regclass AND d.objid=c.oid AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.refobjid=e.oid AND d.deptype='e'
        WHERE e.extname='pg_stat_statements' AND c.relname='pg_stat_statements_info') AS pgss_info
      FROM pg_catalog.pg_database d WHERE d.datname=pg_catalog.current_database()", iso("pg_catalog.pg_postmaster_start_time()"), iso("pg_catalog.clock_timestamp()"));
    let row = one(reader, &sql).await?;
    crate::probe::ensure_postgres(&text(&row, "build")?)?;
    let database = DatabaseIdentity::server(Engine::Postgres, &text(&row, "database")?, host, port, None);
    let statements = match (row.text("pgss_schema"), row.text("pgss_columns")) {
        (Some(schema), Some(columns)) => Some(StatementCatalog {
            schema,
            columns: serde_json::from_str(&columns).map_err(|_| invalid())?,
            info: flag(&row, "pgss_info")?,
        }),
        (None, None) => None,
        _ => return Err(invalid()),
    };
    let block_bytes = u32::try_from(integer(&row, "block_bytes")?).map_err(|_| invalid())?;
    if block_bytes == 0 {
        return Err(invalid());
    }
    Ok(Metadata {
        database,
        database_oid: integer(&row, "database_oid")?.to_string(),
        version: u32::try_from(integer(&row, "version")?).map_err(|_| invalid())?,
        started_at: text(&row, "started_at")?,
        at: text(&row, "observed_at")?,
        block_bytes,
        full_visibility: flag(&row, "full_visibility")?,
        standby: flag(&row, "standby")?,
        statements,
    })
}
