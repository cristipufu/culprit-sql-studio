//! Where a database runs, and what that changes about reading it and advising on it.
//!
//! A provider detects the platform once (`ServerInfo::platform`); checks and the engine ask it what they need (how a
//! server setting is changed, whether the cloud measures the server, how to load a preloaded library) instead of
//! matching its name. Adding a platform means a variant here, its answers below, and its detection in the provider
//! that can see it: nothing else matches on platform names.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::cloud::CloudProvider;
use crate::model::Engine;
use crate::t;

/// A managed service a database runs on. A server you run yourself has none.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum Platform {
    /// Amazon RDS for PostgreSQL, MySQL, MariaDB or SQL Server.
    AmazonRds,
    /// Amazon Aurora, PostgreSQL- or MySQL-compatible.
    AmazonAurora,
    /// Azure SQL Database: one database (or an elastic pool), without the instance around it.
    AzureSqlDatabase,
    AzureSqlManagedInstance,
    /// Azure Database for PostgreSQL, MySQL or MariaDB.
    AzureDatabase,
    GoogleCloudSql,
    GoogleAlloyDb,
    Neon,
    Supabase,
    /// Tiger Cloud, formerly Timescale Cloud. The TimescaleDB extension also runs on self-managed PostgreSQL.
    TigerData,
    /// MariaDB Cloud (formerly SkySQL).
    MariaDbCloud,
}

/// Every platform, for tests and listings.
pub const PLATFORMS: [Platform; 11] = [
    Platform::AmazonRds,
    Platform::AmazonAurora,
    Platform::AzureSqlDatabase,
    Platform::AzureSqlManagedInstance,
    Platform::AzureDatabase,
    Platform::GoogleCloudSql,
    Platform::GoogleAlloyDb,
    Platform::Neon,
    Platform::Supabase,
    Platform::TigerData,
    Platform::MariaDbCloud,
];

impl Platform {
    /// How a person names it: "Amazon RDS", "Azure Database for PostgreSQL".
    pub fn label(self, engine: Engine) -> &'static str {
        match self {
            Platform::AmazonRds => "Amazon RDS",
            Platform::AmazonAurora => "Amazon Aurora",
            Platform::AzureSqlDatabase => "Azure SQL Database",
            Platform::AzureSqlManagedInstance => "Azure SQL Managed Instance",
            Platform::AzureDatabase => match engine {
                Engine::Postgres => "Azure Database for PostgreSQL",
                Engine::Mysql => "Azure Database for MySQL",
                Engine::Mssql | Engine::Sqlite => "Azure SQL",
                Engine::Yugabyte => "YugabyteDB on Azure",
            },
            Platform::GoogleCloudSql => "Google Cloud SQL",
            Platform::GoogleAlloyDb => "Google AlloyDB",
            Platform::Neon => "Neon",
            Platform::Supabase => "Supabase",
            Platform::TigerData => "Tiger Data",
            Platform::MariaDbCloud => "MariaDB Cloud",
        }
    }

    /// Where a server setting is changed when SQL can't (no superuser, no configuration file): a sentence's object,
    /// "Set work_mem to 64MB in {this}." `None` where the engine's own SQL does it (sp_configure on a managed
    /// instance) or where server settings aren't the person's to change (Azure SQL Database).
    pub fn settings_control(self) -> Option<String> {
        Some(match self {
            Platform::AmazonRds => t!("model.platform.settings.amazon_rds"),
            Platform::AmazonAurora => t!("model.platform.settings.amazon_aurora"),
            Platform::AzureDatabase => t!("model.platform.settings.azure_database"),
            Platform::GoogleCloudSql => t!("model.platform.settings.google_cloud_sql"),
            Platform::GoogleAlloyDb => t!("model.platform.settings.google_alloydb"),
            Platform::Neon => t!("model.platform.settings.neon"),
            Platform::Supabase => t!("model.platform.settings.supabase"),
            Platform::TigerData => t!("model.platform.settings.tiger_data"),
            Platform::MariaDbCloud => t!("model.platform.settings.mariadb_cloud"),
            Platform::AzureSqlDatabase | Platform::AzureSqlManagedInstance => return None,
        })
    }

    /// How to add a library to `shared_preload_libraries` (pg_stat_statements) where that isn't a configuration file.
    pub fn preload_library(self, library: &str) -> Option<String> {
        self.preload_library_message(library).map(|message| message.render())
    }

    pub fn preload_library_message(self, library: &str) -> Option<crate::i18n::Message> {
        Some(match self {
            Platform::AmazonRds | Platform::AmazonAurora => {
                crate::message!("model.platform.preload.parameter_group", library = library)
            }
            Platform::AzureDatabase => crate::message!("model.platform.preload.azure_database", library = library),
            Platform::GoogleCloudSql | Platform::Neon | Platform::Supabase => {
                crate::message!(
                    "model.platform.preload.preloaded",
                    library = library,
                    platform = self.label(Engine::Postgres)
                )
            }
            Platform::GoogleAlloyDb => crate::message!("model.platform.preload.google_alloydb", library = library),
            Platform::TigerData => crate::message!("model.platform.preload.tiger_data", library = library),
            Platform::AzureSqlDatabase | Platform::AzureSqlManagedInstance | Platform::MariaDbCloud => return None,
        })
    }

    /// Storage that applies the WAL itself (Aurora, AlloyDB, Neon): a checkpoint doesn't write the data out in one burst,
    /// and max_wal_size is the service's, so forced checkpoints are no finding there.
    pub fn applies_wal_in_storage(self) -> bool {
        matches!(self, Platform::AmazonAurora | Platform::GoogleAlloyDb | Platform::Neon)
    }

    /// The cloud whose own monitoring measures the server (CPU, credits, IOPS against limits), when Culprit reads it.
    pub fn metrics(self) -> Option<CloudProvider> {
        match self {
            Platform::AmazonRds | Platform::AmazonAurora => Some(CloudProvider::Aws),
            Platform::AzureSqlDatabase | Platform::AzureSqlManagedInstance | Platform::AzureDatabase => {
                Some(CloudProvider::Azure)
            }
            _ => None,
        }
    }

    /// Azure SQL Database: the database without its instance. Instance-wide views are its pool's or missing, and
    /// server settings are Microsoft's.
    pub fn is_azure_sql_database(self) -> bool {
        self == Platform::AzureSqlDatabase
    }

    /// One of Azure's SQL Server services (a database or a managed instance).
    pub fn is_azure_sql(self) -> bool {
        matches!(self, Platform::AzureSqlDatabase | Platform::AzureSqlManagedInstance)
    }

    /// One of Azure's services, whatever the engine.
    pub fn is_azure(self) -> bool {
        matches!(self, Platform::AzureSqlDatabase | Platform::AzureSqlManagedInstance | Platform::AzureDatabase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platform_without_superuser_names_its_control() {
        assert!(Platform::AmazonRds.settings_control().unwrap().contains("parameter group"));
        assert!(Platform::GoogleCloudSql.settings_control().unwrap().contains("database flags"));
        assert_eq!(Platform::AzureSqlManagedInstance.settings_control(), None);
        assert!(Platform::Supabase.preload_library("pg_stat_statements").unwrap().contains("CREATE EXTENSION"));
    }
}
