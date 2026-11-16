use culprit_core::{Capability, Engine, Error, Result, ServerInfo, t};
#[cfg(test)]
use tokio_postgres::Client;
#[cfg(test)]
mod tests;

pub(crate) const ALL_STATS: &str = "(EXISTS(SELECT 1 FROM pg_roles WHERE rolname=current_user AND rolsuper) OR pg_has_role(current_user,'pg_read_all_stats','USAGE'))";

// Capability, not a server-version heuristic: an older extension can retain temporary blocks without this view.
// Only the extension-owned native C function/view with access for this login can supply reset evidence.
const STATEMENT_INFO: &str = "EXISTS(
    SELECT 1 FROM pg_catalog.pg_extension e
    JOIN pg_catalog.pg_class c ON c.relnamespace=e.extnamespace AND c.relname='pg_stat_statements_info' AND c.relkind='v'
    JOIN pg_catalog.pg_depend cd ON cd.classid='pg_catalog.pg_class'::regclass AND cd.objid=c.oid
      AND cd.refclassid='pg_catalog.pg_extension'::regclass AND cd.refobjid=e.oid AND cd.deptype='e'
    JOIN pg_catalog.pg_roles cr ON cr.oid=c.relowner AND cr.rolsuper
    JOIN pg_catalog.pg_proc p ON p.pronamespace=e.extnamespace AND p.proname='pg_stat_statements_info'
      AND p.pronargs=0 AND NOT p.prosecdef
    JOIN pg_catalog.pg_language l ON l.oid=p.prolang AND l.lanname='c'
    JOIN pg_catalog.pg_depend pd ON pd.classid='pg_catalog.pg_proc'::regclass AND pd.objid=p.oid
      AND pd.refclassid='pg_catalog.pg_extension'::regclass AND pd.refobjid=e.oid AND pd.deptype='e'
    JOIN pg_catalog.pg_roles pr ON pr.oid=p.proowner AND pr.rolsuper
    JOIN pg_catalog.pg_rewrite r ON r.ev_class=c.oid
    JOIN pg_catalog.pg_depend rd ON rd.classid='pg_catalog.pg_rewrite'::regclass AND rd.objid=r.oid
      AND rd.refclassid='pg_catalog.pg_proc'::regclass AND rd.refobjid=p.oid
    WHERE e.extname='pg_stat_statements'
      AND pg_catalog.has_schema_privilege(e.extnamespace,'USAGE')
      AND pg_catalog.has_table_privilege(c.oid,'SELECT')
      AND pg_catalog.has_function_privilege(p.oid,'EXECUTE'))";

pub(crate) async fn require_all_stats_from(reader: &mut impl culprit_postgres::wire::QueryRead) -> Result<()> {
    if crate::read::query_one(reader, &format!("SELECT {ALL_STATS} AS allowed"))
        .await?
        .is_some_and(|r| r.flag("allowed"))
    {
        Ok(())
    } else {
        Err(Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            t!("provider.yugabyte.error.privilege_changed_history"),
        ))
    }
}

#[derive(Clone)]
pub(crate) struct Probe {
    pub now: String,
    pub started: Option<String>,
    pub version: String,
    pub compatibility: u32,
    pub database: String,
    pub oid: u32,
    pub node: Option<String>,
    pub cluster: bool,
    pub global: bool,
    pub statements: bool,
    pub statement_schema: Option<String>,
    pub statement_info: bool,
    pub metrics: bool,
    pub rpc_metrics: bool,
    pub statement_role: Option<u32>,
}

impl Probe {
    pub fn server(&self) -> ServerInfo {
        ServerInfo {
            engine: Engine::Yugabyte,
            product: format!(
                "YugabyteDB {} (YSQL PostgreSQL {})",
                self.version,
                format_args!("{}.{}", self.compatibility / 10000, self.compatibility % 10000)
            ),
            version: self.version.clone(),
            edition: None,
            cloud: None,
            platform: None,
            database: self.database.clone(),
            database_size_bytes: None,
            started_at: self.started.clone(),
            read_only_session: true,
            capabilities: vec![Capability::TableStats, Capability::Statistics],
            missing: vec![],
        }
    }
}

pub(crate) fn node_id(value: &str) -> Option<String> {
    let value = value.replace('-', "").to_ascii_lowercase();
    (value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
}

pub(crate) fn timestamp(expression: &str) -> String {
    format!("to_char(({expression}) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
}

#[cfg(test)]
pub(crate) async fn read(client: &Client) -> Result<Probe> {
    read_from(&mut &*client).await
}
async fn one(reader: &mut impl culprit_postgres::wire::QueryRead, sql: &str) -> Result<culprit_postgres::wire::Row> {
    let mut rows = reader.query(sql, 2).await?;
    if rows.len() != 1 {
        return Err(Error::invalid(t!("provider.yugabyte.error.no_identity")));
    }
    Ok(rows.remove(0))
}
pub(crate) async fn read_from(reader: &mut impl culprit_postgres::wire::QueryRead) -> Result<Probe> {
    // The initial probe is deliberately independent of all PostgreSQL/DocDB size and heap APIs.
    let row = one(
        reader,
        &format!(
            "SELECT version() AS version, current_setting('server_version_num') AS compatibility,
        current_database() AS database, (SELECT oid::text FROM pg_database WHERE datname=current_database()) AS oid,
        {} AS now, {} AS started, current_setting('yb_enable_global_views',true) AS global_views,
        (SELECT n.nspname FROM pg_extension e JOIN pg_namespace n ON n.oid=e.extnamespace
          WHERE e.extname='pg_stat_statements') AS statement_schema,
        {STATEMENT_INFO} AS statement_info,
        to_regclass('pg_catalog.\"gv$pg_stat_statements\"') IS NOT NULL AS cluster_statements,
        current_setting('yb_enable_pg_stat_statements_docdb_metrics',true) AS metrics,
        current_setting('yb_enable_pg_stat_statements_rpc_stats',true) AS rpc_metrics,
        (SELECT oid::text FROM pg_roles WHERE rolname=current_user) AS role_oid,
        {ALL_STATS} AS all_stats",
            timestamp("clock_timestamp()"),
            timestamp("pg_postmaster_start_time()")
        ),
    )
    .await?;
    let version = row.string("version");
    let version = version
        .split_once("-YB-")
        .map(|(_, v)| v.split_whitespace().next().unwrap_or(v))
        .ok_or_else(|| {
            Error::invalid(t!("provider.yugabyte.error.not_yugabyte"))
                .with_hint(t!("provider.yugabyte.error.not_yugabyte_hint"))
        })?
        .to_string();
    let statement_info = match row.value("statement_info") {
        Some(Some("t" | "true")) => true,
        Some(Some("f" | "false")) => false,
        _ => return Err(Error::invalid(t!("collection.invalid_source"))),
    };
    let node = match one(reader, "SELECT yb_get_local_tserver_uuid()::text AS node").await {
        Ok(row) => node_id(&row.string("node")),
        Err(error)
            if error.detail.is_none()
                && matches!(
                    error.code,
                    culprit_core::ErrorCode::PermissionDenied | culprit_core::ErrorCode::Unsupported
                ) =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    Ok(Probe {
        now: row.string("now"),
        started: row.text("started"),
        version,
        compatibility: row
            .string("compatibility")
            .parse()
            .map_err(|_| Error::invalid(t!("provider.yugabyte.error.invalid_compatibility")))?,
        database: row.string("database"),
        oid: row.string("oid").parse().map_err(|_| Error::invalid(t!("provider.yugabyte.error.invalid_oid")))?,
        node,
        cluster: row.flag("global_views") && row.flag("cluster_statements"),
        global: row.flag("global_views"),
        statements: row.text("statement_schema").is_some(),
        statement_schema: row.text("statement_schema"),
        statement_info,
        metrics: row.flag("metrics"),
        rpc_metrics: row.flag("rpc_metrics"),
        statement_role: if row.flag("all_stats") {
            None
        } else {
            Some(row.string("role_oid").parse().map_err(|_| Error::invalid(t!("provider.yugabyte.error.no_role")))?)
        },
    })
}
