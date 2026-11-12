//! Server identity, capabilities and source availability, before collection begins.
use crate::{
    connect::sql_state,
    rows::{iso, query_one, quote_ident},
};
use culprit_core::i18n::Message;
use culprit_core::{Capability, Engine, MissingCapability, Platform, Result, ServerInfo, t};
use tokio_postgres::Client;

const CREATE_STATEMENTS_EXTENSION: &str = "CREATE EXTENSION pg_stat_statements;";

/// What the probe learned about the server, shared by every collector.
pub struct Probe {
    pub version_num: u32,
    pub version: String,
    pub timescale_version: Option<String>,
    pub timescale_schema: Option<String>,
    pub database: String,
    pub size_bytes: Option<f64>,
    pub started_at: Option<String>,
    pub now: String,
    pub user: String,
    pub privileged: bool,
    pub platform: Option<Platform>,
    pub statements: StatementsAccess,
    /// The server already loads pg_stat_statements (shared_preload_libraries), so it has been recording all along and
    /// only the extension's view is missing.
    pub preloaded: Option<bool>,
    /// This login can create the extension (superuser, a cloud admin role, or CREATE on the database).
    pub can_create_extension: bool,
    /// The server is a standby replaying another's WAL (`pg_is_in_recovery()`): its usage counters count only the reads
    /// run on it.
    pub standby: bool,
    /// The login can do more than read statistics (`crate::trust::elevated_sql`): it doesn't plan while another role's code
    /// is in reach.
    pub elevated: bool,
}

pub enum StatementsAccess {
    Usable { relation: String, info: Option<String> },
    NotInstalled,
    NotLoaded(String),
    Failed(String),
}

impl Probe {
    pub fn product(&self) -> String {
        let short = self.version.split_whitespace().next().unwrap_or(&self.version);
        match &self.timescale_version {
            Some(version) => format!("TimescaleDB {version} (PostgreSQL {short})"),
            None => format!("PostgreSQL {short}"),
        }
    }

    pub fn generic_plans(&self) -> bool {
        self.version_num >= crate::generic::MIN_VERSION
    }

    pub(crate) fn statements_usable(&self) -> bool {
        matches!(self.statements, StatementsAccess::Usable { .. })
    }

    pub fn server_info(&self) -> ServerInfo {
        let mut capabilities = vec![
            Capability::TableStats,
            Capability::IndexUsage,
            Capability::Statistics,
            Capability::Sessions,
            Capability::Showplan,
        ];
        let mut missing = Vec::new();
        match &self.statements {
            StatementsAccess::Usable { .. } => capabilities.push(Capability::PgStatStatements),
            StatementsAccess::NotInstalled => missing.push(self.statements_missing(
                Capability::PgStatStatements,
                culprit_core::message!("provider.postgres.capability.statements.not_installed"),
            )),
            StatementsAccess::NotLoaded(message) => missing.push(self.statements_missing(
                Capability::PgStatStatements,
                culprit_core::message!("provider.postgres.capability.statements.not_loaded", error = message),
            )),
            StatementsAccess::Failed(message) => missing.push(MissingCapability::sql(
                Capability::PgStatStatements,
                culprit_core::message!("provider.postgres.capability.statements.failed", error = message),
                format!("GRANT pg_read_all_stats TO {};", quote_ident(&self.user)),
            )),
        }
        if self.generic_plans() {
            capabilities.push(Capability::GenericPlans);
            if self.statements_usable() {
                capabilities.push(Capability::QueryPlans);
            } else {
                missing.push(self.statements_missing(
                    Capability::QueryPlans,
                    culprit_core::message!("provider.postgres.capability.plans.no_statements"),
                ));
            }
        } else {
            let reason = culprit_core::message!(
                "provider.postgres.capability.generic_plans.old_version",
                version = self.version.split_whitespace().next().unwrap_or(&self.version)
            );
            let fix = culprit_core::message!("provider.postgres.capability.generic_plans.upgrade");
            missing.push(MissingCapability::new(Capability::GenericPlans, reason.clone()).with_fix(fix.clone()));
            missing.push(MissingCapability::new(Capability::QueryPlans, reason).with_fix(fix));
        }
        if self.privileged {
            capabilities.push(Capability::WaitStats);
        } else {
            missing.push(MissingCapability::sql(
                Capability::WaitStats,
                culprit_core::message!("provider.postgres.capability.waits.hidden"),
                format!("GRANT pg_monitor TO {};", quote_ident(&self.user)),
            ));
        }
        ServerInfo {
            engine: Engine::Postgres,
            product: self.product(),
            version: self.version.clone(),
            edition: None,
            cloud: self.platform.map(|p| p.label(Engine::Postgres).to_string()),
            platform: self.platform,
            database: self.database.clone(),
            database_size_bytes: self.size_bytes,
            started_at: self.started_at.clone(),
            read_only_session: true,
            capabilities,
            missing,
        }
    }

    fn statements_missing(&self, capability: Capability, reason: Message) -> MissingCapability {
        let missing = MissingCapability::new(capability, reason).with_fix(self.statements_fix());
        if self.preloaded != Some(true) || !matches!(self.statements, StatementsAccess::NotInstalled) {
            return missing;
        }
        let mut detail = Vec::new();
        if !self.can_create_extension {
            detail.push(culprit_core::message!("provider.postgres.capability.statements.create_permission"));
        }
        detail.push(culprit_core::message!("provider.postgres.capability.statements.preloaded"));
        missing.with_action(CREATE_STATEMENTS_EXTENSION, Some(Message::paragraph(detail)))
    }

    fn statements_fix(&self) -> Message {
        if self.preloaded == Some(true) {
            let create = if self.can_create_extension {
                culprit_core::message!(
                    "provider.postgres.capability.statements.create",
                    sql = CREATE_STATEMENTS_EXTENSION
                )
            } else {
                culprit_core::message!(
                    "provider.postgres.capability.statements.create_as_admin",
                    sql = CREATE_STATEMENTS_EXTENSION
                )
            };
            return Message::paragraph(vec![
                create,
                culprit_core::message!("provider.postgres.capability.statements.preloaded"),
            ]);
        }
        if self.preloaded.is_none() {
            return culprit_core::message!("provider.postgres.capability.statements.preload_unknown");
        }
        self.platform.and_then(|p| p.preload_library_message("pg_stat_statements")).unwrap_or_else(|| {
            culprit_core::message!("provider.postgres.capability.statements.restart", sql = CREATE_STATEMENTS_EXTENSION)
        })
    }
}

pub(crate) fn ensure_postgres(build: &str) -> Result<()> {
    if build.contains("-YB-") {
        return Err(culprit_core::Error::invalid(t!("provider.postgres.error.yugabyte"))
            .with_hint(t!("provider.postgres.error.yugabyte_hint")));
    }
    Ok(())
}

pub async fn probe(client: &Client, spec_host: &str) -> Result<Probe> {
    let build = query_one(client, "SELECT version() AS build")
        .await?
        .ok_or_else(|| culprit_core::Error::internal(t!("provider.postgres.error.probe_no_version")))?;
    ensure_postgres(&build.string("build"))?;
    let sql = format!(
        "SELECT current_setting('server_version_num') AS version_num,
                current_setting('server_version') AS version,
                (SELECT extversion FROM pg_extension WHERE extname = 'timescaledb') AS timescale_version,
                (SELECT n.nspname FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
                  WHERE e.extname = 'timescaledb') AS timescale_schema,
                current_database() AS database,
                pg_database_size(current_database()) AS size_bytes,
                {started} AS started_at,
                {now} AS now,
                current_user AS username,
                (SELECT rolsuper FROM pg_roles WHERE rolname = current_user)
                  OR pg_has_role(current_user, 'pg_read_all_stats', 'MEMBER') AS privileged,
                (SELECT n.nspname FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
                  WHERE e.extname = 'pg_stat_statements') AS pgss_schema,
                (SELECT string_agg(DISTINCT split_part(name, '.', 1), ',') FROM pg_settings
                  WHERE name ~ '^(rds|azure|cloudsql|alloydb|neon)\\.') AS cloud_settings,
                EXISTS (SELECT 1 FROM pg_proc WHERE proname = 'aurora_version') AS aurora,
                'pg_stat_statements' = ANY (string_to_array(replace((SELECT setting FROM pg_settings WHERE name = 'shared_preload_libraries'), ' ', ''), ',')) AS preloaded,
                (SELECT rolsuper FROM pg_roles WHERE rolname = current_user)
                  OR EXISTS (SELECT 1 FROM pg_roles r WHERE r.rolname IN ('rds_superuser', 'azure_pg_admin', 'cloudsqlsuperuser')
                               AND pg_has_role(current_user, r.oid, 'MEMBER'))
                  OR has_database_privilege(current_database(), 'CREATE') AS can_create_extension,
                pg_is_in_recovery() AS in_recovery,
                {elevated} AS elevated",
        started = iso("pg_postmaster_start_time()"),
        elevated = crate::trust::elevated_sql(),
        now = iso("now()"),
    );
    let row = query_one(client, &sql)
        .await?
        .ok_or_else(|| culprit_core::Error::internal(t!("provider.postgres.error.probe_empty")))?;
    let version_num = row.number("version_num").unwrap_or(0.0) as u32;
    let platform = detect_platform(&row.string("cloud_settings"), row.flag("aurora"), spec_host);
    let mut probe = Probe {
        version_num,
        version: row.string("version"),
        timescale_version: row.text("timescale_version"),
        timescale_schema: row.text("timescale_schema"),
        database: row.string("database"),
        size_bytes: row.number("size_bytes"),
        started_at: row.text("started_at"),
        now: row.string("now"),
        user: row.string("username"),
        privileged: row.flag("privileged"),
        platform,
        statements: StatementsAccess::NotInstalled,
        preloaded: row.text("preloaded").map(|_| row.flag("preloaded")),
        can_create_extension: row.flag("can_create_extension"),
        standby: row.flag("in_recovery"),
        elevated: row.flag("elevated"),
    };
    if let Some(schema) = row.text("pgss_schema") {
        probe.statements = statements_access(client, &schema, version_num).await;
    }
    Ok(probe)
}

async fn statements_access(client: &Client, schema: &str, version_num: u32) -> StatementsAccess {
    let relation = format!("{}.pg_stat_statements", quote_ident(schema));
    if let Err(e) = client.simple_query(&crate::rows::tagged(&format!("SELECT 1 FROM {relation} LIMIT 1"))).await {
        let message = e.as_db_error().map(|d| d.message().to_string()).unwrap_or_else(|| e.to_string());
        return match sql_state(&e) {
            Some("55000") => StatementsAccess::NotLoaded(message),
            _ => StatementsAccess::Failed(message),
        };
    }
    let mut info = None;
    if version_num >= 140_000 {
        let sql =
            format!("SELECT {} AS reset FROM {}.pg_stat_statements_info", iso("stats_reset"), quote_ident(schema));
        if let Ok(Some(row)) = query_one(client, &sql).await {
            info = row.text("reset");
        }
    }
    StatementsAccess::Usable { relation, info }
}

/// The managed service from what the server says about itself (its settings' prefixes, Aurora's own function), else
/// from the host name.
fn detect_platform(settings: &str, aurora: bool, host: &str) -> Option<Platform> {
    let host = host.to_ascii_lowercase();
    let has = |prefix: &str| settings.split(',').any(|s| s == prefix);
    Some(if aurora {
        Platform::AmazonAurora
    } else if has("rds") || host.ends_with(".rds.amazonaws.com") {
        Platform::AmazonRds
    } else if has("azure") || host.ends_with(".postgres.database.azure.com") {
        Platform::AzureDatabase
    } else if has("alloydb") {
        Platform::GoogleAlloyDb
    } else if has("cloudsql") {
        Platform::GoogleCloudSql
    } else if has("neon") || host.ends_with(".neon.tech") {
        Platform::Neon
    } else if host.ends_with(".tsdb.cloud.timescale.com") || host.ends_with(".tsdb.forge.timescale.com") {
        Platform::TigerData
    } else if host.contains("supabase.co") || host.contains("supabase.com") {
        Platform::Supabase
    } else {
        return None;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_detection() {
        assert_eq!(detect_platform("", false, "example.tsdb.cloud.timescale.com"), Some(Platform::TigerData));
        assert_eq!(detect_platform("timescaledb", false, "localhost"), None);
        assert_eq!(detect_platform("rds", false, "x"), Some(Platform::AmazonRds));
        assert_eq!(detect_platform("", true, "x"), Some(Platform::AmazonAurora));
        assert_eq!(detect_platform("", false, "a.postgres.database.azure.com"), Some(Platform::AzureDatabase));
        assert_eq!(detect_platform("alloydb", false, "10.0.0.5"), Some(Platform::GoogleAlloyDb));
        assert_eq!(detect_platform("", false, "ep-cool-1.eu-central-1.aws.neon.tech"), Some(Platform::Neon));
        assert_eq!(detect_platform("", false, "localhost"), None);
    }

    fn probe(statements: StatementsAccess) -> Probe {
        Probe {
            version_num: 160_000,
            version: "16.2 (Debian 16.2-1)".into(),
            timescale_version: None,
            timescale_schema: None,
            database: "shop".into(),
            size_bytes: None,
            started_at: None,
            now: "2026-11-08T10:00:00Z".into(),
            user: "Monitor".into(),
            privileged: true,
            platform: None,
            statements,
            preloaded: Some(false),
            can_create_extension: true,
            standby: false,
            elevated: false,
        }
    }

    fn missing(probe: &Probe, capability: Capability) -> (String, Option<String>) {
        let info = probe.server_info();
        let m = info.missing.iter().find(|m| m.capability == capability).unwrap();
        (m.reason.clone(), m.fix.clone())
    }

    #[test]
    fn the_extension_action_requires_confirmed_preload_and_an_absent_extension() {
        for language in culprit_core::i18n::languages() {
            culprit_core::i18n::scope(&language.code, || {
                for preloaded in [None, Some(false), Some(true)] {
                    for absent in [false, true] {
                        let state = if absent {
                            StatementsAccess::NotInstalled
                        } else {
                            StatementsAccess::NotLoaded("not loaded".into())
                        };
                        let mut probe = probe(state);
                        probe.preloaded = preloaded;
                        let server = probe.server_info();
                        let missing =
                            server.missing.iter().find(|m| m.capability == Capability::PgStatStatements).unwrap();
                        assert_eq!(missing.action.is_some(), preloaded == Some(true) && absent);
                        if let Some(action) = &missing.action {
                            assert_eq!(action.sql, CREATE_STATEMENTS_EXTENSION);
                        }
                    }
                }
            });
        }
    }

    // Why each capability is locked and how to unlock it, word for word.
    #[test]
    fn locked_capabilities_in_english() {
        let restart = "Add pg_stat_statements to shared_preload_libraries, restart, then run CREATE EXTENSION \
                       pg_stat_statements; in this database.";
        let not_installed = probe(StatementsAccess::NotInstalled);
        assert_eq!(
            missing(&not_installed, Capability::PgStatStatements),
            (
                "The pg_stat_statements extension is not installed in this database, so Culprit cannot see which \
                 queries use the most time."
                    .into(),
                Some(restart.into())
            )
        );
        assert_eq!(
            missing(&not_installed, Capability::QueryPlans),
            (
                "Plans for the heaviest queries come from pg_stat_statements, which is not available.".into(),
                Some(restart.into())
            )
        );
        let preloaded = "The server already loads it, so no restart is needed and the statistics it has been recording \
                         since the last restart appear right away.";
        let mut p = probe(StatementsAccess::NotInstalled);
        p.preloaded = Some(true);
        assert_eq!(
            missing(&p, Capability::PgStatStatements).1.unwrap(),
            format!("Run CREATE EXTENSION pg_stat_statements; in this database. {preloaded}")
        );
        p.can_create_extension = false;
        assert_eq!(
            missing(&p, Capability::PgStatStatements).1.unwrap(),
            format!(
                "Run CREATE EXTENSION pg_stat_statements; in this database as a user who can create extensions (an \
                 admin role, or the database owner). {preloaded}"
            )
        );
        p.preloaded = None;
        assert_eq!(
            missing(&p, Capability::PgStatStatements).1.unwrap(),
            "This login cannot read shared_preload_libraries. Ask an administrator to verify whether \
             pg_stat_statements is already loaded. If loaded, creating its extension in this database exposes its \
             statistics without a restart; otherwise configure the preload and restart first."
        );
        let not_loaded = probe(StatementsAccess::NotLoaded("it must be loaded via shared_preload_libraries".into()));
        assert_eq!(
            missing(&not_loaded, Capability::PgStatStatements).0,
            "pg_stat_statements is installed but not loaded by the server (it must be loaded via \
             shared_preload_libraries)."
        );
        let failed = probe(StatementsAccess::Failed("permission denied for view pg_stat_statements".into()));
        assert_eq!(
            missing(&failed, Capability::PgStatStatements),
            (
                "Reading pg_stat_statements failed: permission denied for view pg_stat_statements".into(),
                Some("GRANT pg_read_all_stats TO \"Monitor\";".into())
            )
        );
        let mut old = probe(StatementsAccess::Usable { relation: "public.pg_stat_statements".into(), info: None });
        old.version_num = 110_022;
        old.version = "11.22".into();
        let upgrade = (
            "Planning the heaviest queries without running them needs PostgreSQL 12 or newer. This server runs 11.22."
                .to_string(),
            Some("Upgrade to PostgreSQL 12 or newer, or explain a query with sample values in the editor.".to_string()),
        );
        assert_eq!(missing(&old, Capability::GenericPlans), upgrade);
        assert_eq!(missing(&old, Capability::QueryPlans), upgrade);
        old.privileged = false;
        assert_eq!(
            missing(&old, Capability::WaitStats),
            (
                "Other sessions' state and waits are hidden from this login.".into(),
                Some("GRANT pg_monitor TO \"Monitor\";".into())
            )
        );
    }
}
