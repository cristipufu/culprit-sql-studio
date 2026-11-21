//! Exact nonce-owned metadata objects; these controls never run on a production endpoint.
use super::*;
use crate::{
    conn::{MsClient, SessionKind},
    test_support::{config, connect, verify_owned_source},
};
use tiberius::Config;

mod control;
mod runtime;
pub(super) mod temporal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopulationWork {
    NotStarted,
    MayBeRunning,
    StopCommandCompleted,
}
impl PopulationWork {
    fn requires_supervisor(self) -> bool {
        self != Self::NotStarted
    }
}

struct NativeIds {
    database: u32,
    plain: u32,
    indexed: u32,
    unique_index: u32,
    catalog: Option<u32>,
    last_crawl_completed: Option<bool>,
    created_without_offset: String,
    modified_without_offset: String,
}

pub(super) struct Fixture<'a> {
    owned: &'a OwnedSourceFixture,
    prefix: String,
    owns_objects: bool,
    source_capture_unsettled: bool,
    population_capture_attempts: u32,
    population_captures_completed: u32,
    control_available: bool,
    control_connections: u32,
    control_sql_attempts: u32,
    population_work: PopulationWork,
    ids: Option<NativeIds>,
    temporal: temporal::Objects,
}
impl<'a> Fixture<'a> {
    pub(super) fn new(owned: &'a OwnedSourceFixture) -> Self {
        let owner = std::env::var("CULPRIT_SMOKE_OWNER").unwrap();
        let prefix =
            format!("culprit_fts_{owner}_{}_{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap());
        Self {
            owned,
            prefix,
            owns_objects: false,
            source_capture_unsettled: false,
            population_capture_attempts: 0,
            population_captures_completed: 0,
            control_available: true,
            control_connections: 0,
            control_sql_attempts: 0,
            population_work: PopulationWork::NotStarted,
            ids: None,
            temporal: temporal::Objects::default(),
        }
    }
    fn name(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.prefix)
    }
    pub(super) fn indexed_table(&self) -> String {
        self.name("indexed")
    }
    pub(super) fn owns(&self, schema: &str, table: &str) -> bool {
        schema == "dbo"
            && ([self.name("plain"), self.indexed_table()].iter().any(|name| name == table)
                || self.temporal.claimed && self.temporal_names().iter().any(|name| name == table))
    }
    pub(super) fn config(&self) -> Config {
        config(self.owned.port, &self.owned.database, self.owned.password(), "culprit-full-text-source")
    }
    pub(super) async fn initialize(&mut self, indexed: bool) -> Result<()> {
        let plain = self.name("plain");
        let table = self.indexed_table();
        let catalog = self.name("catalog");
        let user = self.name("viewer");
        let key = self.name("indexed_key");
        let oracle = self.identity_sql();
        let ids = self.control("culprit-full-text-setup", async |commands, owns_objects| {
            let before = commands.row(&format!(
                "SELECT /* culprit fixture precondition */ CONVERT(int,FULLTEXTSERVICEPROPERTY('IsFullTextInstalled')) AS installed, OBJECT_ID(N'dbo.{plain}',N'U') AS plain_id, OBJECT_ID(N'dbo.{table}',N'U') AS indexed_id, USER_ID(N'{user}') AS user_id, (SELECT fulltext_catalog_id FROM sys.fulltext_catalogs WHERE name=N'{catalog}') AS catalog_id"
            )).await;
            for name in ["plain_id", "indexed_id", "user_id", "catalog_id"] {
                assert_eq!(crate::read::optional_integer(&before, name)?, None,
                    "fixture collision: no existing object may be claimed");
            }
            if indexed && crate::read::optional_boolean(&before, "installed")? != Some(true) {
                return Err(culprit_core::Error::new(culprit_core::ErrorCode::Unsupported,
                    "The owned SQL image has no available native full-text installation; enabled/disabled acceptance did not run."));
            }
            *owns_objects = true;
            commands.batch(&format!("CREATE TABLE dbo.[{plain}](id int NOT NULL CONSTRAINT [{plain}_key] PRIMARY KEY CLUSTERED,content nvarchar(1000) NULL); CREATE TABLE dbo.[{table}](id int NOT NULL CONSTRAINT [{key}] PRIMARY KEY CLUSTERED,content nvarchar(1000) NULL);")).await;
            if indexed {
                commands.batch(&format!("CREATE FULLTEXT CATALOG [{catalog}]")).await;
                commands.batch(&format!("CREATE FULLTEXT INDEX ON dbo.[{table}](content LANGUAGE 1033) KEY INDEX [{key}] ON [{catalog}] WITH(CHANGE_TRACKING=OFF,NO POPULATION)")).await;
            }
            commands.batch(&format!("CREATE USER [{user}] WITHOUT LOGIN; GRANT VIEW DEFINITION TO [{user}]; GRANT SELECT ON OBJECT::dbo.[{plain}] TO [{user}]; GRANT SELECT ON OBJECT::dbo.[{table}] TO [{user}]; DENY SELECT ON OBJECT::sys.fulltext_indexes TO [{user}];")).await;
            NativeIds::decode(&commands.row(&oracle).await)
        }).await?;
        self.ids = Some(ids);
        Ok(())
    }
    fn identity_sql(&self) -> String {
        let plain = self.name("plain");
        let table = self.indexed_table();
        let key = self.name("indexed_key");
        let catalog = self.name("catalog");
        format!(
            "SELECT /* culprit fixture identity */ DB_ID() AS database_id, OBJECT_ID(N'dbo.{plain}',N'U') AS plain_id, OBJECT_ID(N'dbo.{table}',N'U') AS indexed_id, (SELECT index_id FROM sys.indexes WHERE object_id=OBJECT_ID(N'dbo.{table}',N'U') AND name=N'{key}') AS unique_index_id, (SELECT fulltext_catalog_id FROM sys.fulltext_catalogs WHERE name=N'{catalog}') AS catalog_id, (SELECT has_crawl_completed FROM sys.fulltext_indexes WHERE object_id=OBJECT_ID(N'dbo.{table}',N'U')) AS last_crawl_completed, (SELECT CONVERT(varchar(40),create_date,126) FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{table}',N'U')) AS created_without_offset, (SELECT CONVERT(varchar(40),modify_date,126) FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{table}',N'U')) AS modified_without_offset"
        )
    }
    pub(super) fn impersonate(&self) -> String {
        format!("EXECUTE AS USER=N'{}'", self.name("viewer"))
    }
    pub(super) async fn disable(&mut self) -> Result<()> {
        let table = self.indexed_table();
        let native = self.control("culprit-full-text-disable", async |commands, _| {
            commands.batch(&format!("ALTER FULLTEXT INDEX ON dbo.[{table}] DISABLE")).await;
            commands.row(&format!("SELECT /* culprit fixture independent flag */ has_crawl_completed AS last_crawl_completed FROM sys.fulltext_indexes WHERE object_id=OBJECT_ID(N'dbo.{table}',N'U')")).await
        }).await;
        self.ids.as_mut().unwrap().last_crawl_completed =
            crate::read::optional_boolean(&native, "last_crawl_completed")?;
        Ok(())
    }
    pub(super) async fn cleanup(&mut self) -> Result<()> {
        if self.source_capture_unsettled || !self.control_available {
            return Err(crate::read::invalid_data());
        }
        if !self.owns_objects {
            return Ok(());
        }
        if self.population_work.requires_supervisor() {
            // STOP acknowledgement is not a native background-idle proof. Only the positively nonce-owned
            // supervisor may destroy this disposable process; no further fixture DDL is issued here.
            eprintln!("owned full-text population requires supervisor destruction: {:?}", self.population_work);
            return Ok(());
        }
        let plain = self.name("plain");
        let table = self.indexed_table();
        let catalog = self.name("catalog");
        let user = self.name("viewer");
        let temporal_cleanup = self.temporal_cleanup_sql();
        self.control("culprit-full-text-cleanup", async |commands, owns_objects| {
            commands.batch(&format!("{temporal_cleanup}IF OBJECT_ID(N'dbo.{table}',N'U') IS NOT NULL DROP TABLE dbo.[{table}]; IF OBJECT_ID(N'dbo.{plain}',N'U') IS NOT NULL DROP TABLE dbo.[{plain}]; IF EXISTS(SELECT 1 FROM sys.fulltext_catalogs WHERE name=N'{catalog}') DROP FULLTEXT CATALOG [{catalog}]; IF USER_ID(N'{user}') IS NOT NULL DROP USER [{user}];")).await;
            *owns_objects = false;
        }).await;
        Ok(())
    }
    pub(super) fn begin_population_capture(&mut self) {
        assert_eq!(self.population_capture_attempts, 0, "one original source capture, never a retry");
        self.population_capture_attempts += 1;
        self.source_capture_unsettled = true;
    }
    pub(super) fn population_capture_settled(&mut self) {
        assert!(self.source_capture_unsettled);
        self.population_captures_completed += 1;
        self.source_capture_unsettled = false;
    }
    pub(super) fn population_target(&self) -> MssqlFullTextPopulationTarget {
        let ids = self.ids.as_ref().unwrap();
        MssqlFullTextPopulationTarget {
            table: MssqlObjectIdentity {
                database_id: ids.database,
                object_id: ids.indexed,
                created_without_offset: ids.created_without_offset.clone(),
            },
            table_modified_without_offset: ids.modified_without_offset.clone(),
            catalog_id: ids.catalog.unwrap(),
            unique_index_id: ids.unique_index,
        }
    }
    pub(super) fn population_captures(&self) -> Result<crate::test_support::owned_captures::OwnedCaptures> {
        crate::test_support::owned_captures::OwnedCaptures::from_pool(
            Arc::new(Pool::new(self.config(), None)),
            self.owned.port,
        )
    }
    pub(super) fn assert_identity(&self, table: &MssqlTableSnapshot) {
        if self.temporal_names().iter().any(|name| name == &table.name) {
            self.assert_temporal_identity(table);
            return;
        }
        let ids = self.ids.as_ref().unwrap();
        assert_eq!(table.identity.database_id, ids.database);
        assert_eq!(table.identity.object_id, if table.name == self.indexed_table() { ids.indexed } else { ids.plain });
        assert!(table.is_valid());
    }
    pub(super) fn assert_available(&self, tables: &Tables, enabled: Option<bool>) {
        assert_eq!(tables.len(), 2);
        for (name, state) in tables {
            assert_eq!(state.availability, MssqlCatalogAvailability::Available);
            if name == &self.indexed_table()
                && let Some(enabled) = enabled
            {
                let ids = self.ids.as_ref().unwrap();
                let index = state.index.as_ref().expect("a genuine owned full-text index");
                assert_eq!(index.enabled, Some(enabled));
                assert_eq!(index.unique_index_id, Some(ids.unique_index));
                assert_eq!(index.catalog_id, ids.catalog);
                assert!(index.catalog_id.is_some());
                assert_eq!(index.change_tracking, Some(MssqlFullTextChangeTracking::Off));
                assert_eq!(
                    index.last_crawl_completed, ids.last_crawl_completed,
                    "native NULL/flag is retained independently; no freshness verdict"
                );
            } else {
                assert!(state.index.is_none(), "available metadata establishes this owned table has no index");
            }
        }
    }
    pub(super) fn assert_denied(&self, tables: &Tables) {
        assert_eq!(tables.len(), 2);
        for state in tables.values() {
            assert_eq!(state.availability, MssqlCatalogAvailability::PermissionDenied);
            assert!(
                state.index.is_none(),
                "NULL optional fields cannot become index absence under unavailable visibility"
            );
        }
    }
}

impl NativeIds {
    fn decode(ids: &crate::value::Rec) -> Result<Self> {
        Ok(Self {
            database: crate::read::integer(ids, "database_id")?.try_into().unwrap(),
            plain: crate::read::integer(ids, "plain_id")?.try_into().unwrap(),
            indexed: crate::read::integer(ids, "indexed_id")?.try_into().unwrap(),
            unique_index: crate::read::integer(ids, "unique_index_id")?.try_into().unwrap(),
            catalog: crate::read::optional_integer(ids, "catalog_id")?.map(|value| value.try_into().unwrap()),
            last_crawl_completed: crate::read::optional_boolean(ids, "last_crawl_completed")?,
            created_without_offset: crate::read::text(ids, "created_without_offset")?,
            modified_without_offset: crate::read::text(ids, "modified_without_offset")?,
        })
    }
}
