//! Exact nonce-owned metadata objects; these controls never run on a production endpoint.
use super::*;
use crate::{
    conn::{MsClient, SessionKind},
    test_support::{batch, config, connect, verify_owned_source},
};
use tiberius::Config;

struct NativeIds {
    database: u32,
    plain: u32,
    indexed: u32,
    unique_index: u32,
    catalog: Option<u32>,
    last_crawl_completed: Option<bool>,
}

pub(super) struct Fixture<'a> {
    owned: &'a OwnedSourceFixture,
    prefix: String,
    owns_objects: bool,
    ids: Option<NativeIds>,
}
impl<'a> Fixture<'a> {
    pub(super) fn new(owned: &'a OwnedSourceFixture) -> Self {
        let owner = std::env::var("CULPRIT_SMOKE_OWNER").unwrap();
        let prefix =
            format!("culprit_fts_{owner}_{}_{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap());
        Self { owned, prefix, owns_objects: false, ids: None }
    }
    fn name(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.prefix)
    }
    pub(super) fn indexed_table(&self) -> String {
        self.name("indexed")
    }
    pub(super) fn owns(&self, schema: &str, table: &str) -> bool {
        schema == "dbo" && [self.name("plain"), self.indexed_table()].iter().any(|name| name == table)
    }
    pub(super) fn config(&self) -> Config {
        config(self.owned.port, &self.owned.database, self.owned.password(), "culprit-full-text-source")
    }
    async fn actor(&self, label: &str) -> MsClient {
        let mut client =
            connect(&config(self.owned.port, &self.owned.database, self.owned.password(), label), SessionKind::User)
                .await
                .unwrap();
        verify_owned_source(&mut client).await.unwrap();
        client
    }
    pub(super) async fn initialize(&mut self, indexed: bool) -> Result<()> {
        let mut client = self.actor("culprit-full-text-setup").await;
        let plain = self.name("plain");
        let table = self.indexed_table();
        let catalog = self.name("catalog");
        let user = self.name("viewer");
        let key = self.name("indexed_key");
        let before = crate::conn::query_one(&mut client, &format!(
            "SELECT /* culprit fixture precondition */ CONVERT(int,FULLTEXTSERVICEPROPERTY('IsFullTextInstalled')) AS installed, OBJECT_ID(N'dbo.{plain}',N'U') AS plain_id, OBJECT_ID(N'dbo.{table}',N'U') AS indexed_id, USER_ID(N'{user}') AS user_id, (SELECT fulltext_catalog_id FROM sys.fulltext_catalogs WHERE name=N'{catalog}') AS catalog_id"
        )).await.unwrap();
        for name in ["plain_id", "indexed_id", "user_id", "catalog_id"] {
            assert_eq!(
                crate::read::optional_integer(&before, name)?,
                None,
                "fixture collision: no existing object may be claimed"
            );
        }
        if indexed && crate::read::optional_boolean(&before, "installed")? != Some(true) {
            client.close().await.unwrap();
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                "The owned SQL image has no available native full-text installation; enabled/disabled acceptance did not run.",
            ));
        }
        self.owns_objects = true;
        batch(&mut client, &format!(
            "CREATE TABLE dbo.[{plain}](id int NOT NULL CONSTRAINT [{plain}_key] PRIMARY KEY CLUSTERED,content nvarchar(1000) NULL); CREATE TABLE dbo.[{table}](id int NOT NULL CONSTRAINT [{key}] PRIMARY KEY CLUSTERED,content nvarchar(1000) NULL);"
        )).await;
        if indexed {
            batch(&mut client, &format!("CREATE FULLTEXT CATALOG [{catalog}]")).await;
            batch(&mut client, &format!("CREATE FULLTEXT INDEX ON dbo.[{table}](content LANGUAGE 1033) KEY INDEX [{key}] ON [{catalog}] WITH(CHANGE_TRACKING=OFF,NO POPULATION)")).await;
        }
        batch(&mut client, &format!(
            "CREATE USER [{user}] WITHOUT LOGIN; GRANT VIEW DEFINITION TO [{user}]; GRANT SELECT ON OBJECT::dbo.[{plain}] TO [{user}]; GRANT SELECT ON OBJECT::dbo.[{table}] TO [{user}]; DENY SELECT ON OBJECT::sys.fulltext_indexes TO [{user}];"
        )).await;
        // Independent fixture oracle, outside source-query accounting. Only these exact owned catalog keys are read.
        let ids = crate::conn::query_one(&mut client, &format!(
            "SELECT /* culprit fixture identity */ DB_ID() AS database_id, OBJECT_ID(N'dbo.{plain}',N'U') AS plain_id, OBJECT_ID(N'dbo.{table}',N'U') AS indexed_id, (SELECT index_id FROM sys.indexes WHERE object_id=OBJECT_ID(N'dbo.{table}',N'U') AND name=N'{key}') AS unique_index_id, (SELECT fulltext_catalog_id FROM sys.fulltext_catalogs WHERE name=N'{catalog}') AS catalog_id, (SELECT has_crawl_completed FROM sys.fulltext_indexes WHERE object_id=OBJECT_ID(N'dbo.{table}',N'U')) AS last_crawl_completed"
        )).await.unwrap();
        self.ids = Some(NativeIds {
            database: crate::read::integer(&ids, "database_id")?.try_into().unwrap(),
            plain: crate::read::integer(&ids, "plain_id")?.try_into().unwrap(),
            indexed: crate::read::integer(&ids, "indexed_id")?.try_into().unwrap(),
            unique_index: crate::read::integer(&ids, "unique_index_id")?.try_into().unwrap(),
            catalog: crate::read::optional_integer(&ids, "catalog_id")?.map(|value| value.try_into().unwrap()),
            last_crawl_completed: crate::read::optional_boolean(&ids, "last_crawl_completed")?,
        });
        client.close().await.unwrap();
        Ok(())
    }
    pub(super) fn impersonate(&self) -> String {
        format!("EXECUTE AS USER=N'{}'", self.name("viewer"))
    }
    pub(super) async fn disable(&mut self) -> Result<()> {
        let mut client = self.actor("culprit-full-text-disable").await;
        batch(&mut client, &format!("ALTER FULLTEXT INDEX ON dbo.[{}] DISABLE", self.indexed_table())).await;
        let native = crate::conn::query_one(&mut client, &format!(
            "SELECT /* culprit fixture independent flag */ has_crawl_completed AS last_crawl_completed FROM sys.fulltext_indexes WHERE object_id=OBJECT_ID(N'dbo.{}',N'U')", self.indexed_table()
        )).await.unwrap();
        self.ids.as_mut().unwrap().last_crawl_completed =
            crate::read::optional_boolean(&native, "last_crawl_completed")?;
        client.close().await.unwrap();
        Ok(())
    }
    pub(super) async fn cleanup(&mut self) -> Result<()> {
        if !self.owns_objects {
            return Ok(());
        }
        let mut client = self.actor("culprit-full-text-cleanup").await;
        let plain = self.name("plain");
        let table = self.indexed_table();
        let catalog = self.name("catalog");
        let user = self.name("viewer");
        // DROP TABLE also removes its own full-text index; no population or global resource is stopped/flushed.
        batch(&mut client, &format!(
            "IF OBJECT_ID(N'dbo.{table}',N'U') IS NOT NULL DROP TABLE dbo.[{table}]; IF OBJECT_ID(N'dbo.{plain}',N'U') IS NOT NULL DROP TABLE dbo.[{plain}]; IF EXISTS(SELECT 1 FROM sys.fulltext_catalogs WHERE name=N'{catalog}') DROP FULLTEXT CATALOG [{catalog}]; IF USER_ID(N'{user}') IS NOT NULL DROP USER [{user}];"
        )).await;
        client.close().await.unwrap();
        self.owns_objects = false;
        Ok(())
    }
    pub(super) fn assert_identity(&self, table: &MssqlTableSnapshot) {
        let ids = self.ids.as_ref().unwrap();
        assert_eq!(table.identity.database_id, ids.database);
        assert_eq!(table.identity.object_id, if table.name == self.indexed_table() { ids.indexed } else { ids.plain });
        assert!(table.is_valid());
    }
    pub(super) fn assert_available(&self, tables: &Tables, enabled: Option<bool>) {
        assert_eq!(tables.len(), 2);
        for (name, state) in tables {
            assert_eq!(state.availability, MssqlFullTextCatalogAvailability::Available);
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
            assert_eq!(state.availability, MssqlFullTextCatalogAvailability::PermissionDenied);
            assert!(
                state.index.is_none(),
                "NULL optional fields cannot become index absence under unavailable visibility"
            );
        }
    }
}
