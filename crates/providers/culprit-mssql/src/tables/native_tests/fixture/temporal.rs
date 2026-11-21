//! Metadata-only temporal objects admitted by the existing nonce/control owner before their first DDL.
use super::super::temporal::RetentionLayout;
use super::*;
use crate::read::{integer, invalid_data, optional_integer, text};

#[derive(Default)]
pub(super) struct Objects {
    pub(super) claimed: bool,
    witness: Option<Witness>,
}
impl Objects {
    pub(super) fn claim(&mut self) {
        assert!(!self.claimed, "one previously absent owned temporal pair");
        self.claimed = true;
    }
}
#[derive(Clone)]
pub(in crate::tables::native_tests) struct Witness {
    scope: Vec<String>,
    pub database: u32,
    pub current: u32,
    pub history: u32,
    pub created: String,
    pub history_created: String,
    pub at: String,
    pub period: Option<i32>,
    pub unit: Option<i32>,
}
impl Witness {
    pub(in crate::tables::native_tests) fn decode(row: &crate::value::Rec, layout: RetentionLayout) -> Result<Self> {
        let id = |name| u32::try_from(integer(row, name)?).map_err(|_| invalid_data());
        let current = id("current_id")?;
        let history = id("history_id")?;
        if current == history
            || integer(row, "current_type")? != 2
            || integer(row, "history_type")? != 1
            || optional_integer(row, "current_history_id")? != Some(i64::from(history))
            || optional_integer(row, "history_history_id")?.is_some()
            || text(row, "updateability")? != "READ_WRITE"
            || integer(row, "metadata_visible")? != 1
            || !matches!(integer(row, "edition")?, 2 | 3 | 4)
        {
            return Err(invalid_data());
        }
        let major = integer(row, "major")?;
        let retention = integer(row, "retention_columns")?;
        if !layout.accepts(major, retention) {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                "The owned temporal fixture does not provide this selector's required native version/layout; acceptance did not run.",
            ));
        }
        let period = crate::read::optional_signed_integer(row, "configured_period")?
            .map(|v| i32::try_from(v).map_err(|_| invalid_data()))
            .transpose()?;
        let unit = crate::read::optional_signed_integer(row, "configured_unit")?
            .map(|v| i32::try_from(v).map_err(|_| invalid_data()))
            .transpose()?;
        let at = text(row, "observed_at")?;
        canonical_time(&at)?;
        let scope =
            ["machine", "native_database", "database_guid", "recovery_fork_guid", "process_started_without_offset"]
                .into_iter()
                .map(|name| text(row, name))
                .collect::<Result<Vec<_>>>()?;
        if scope.iter().any(|v| v.trim().is_empty()) {
            return Err(invalid_data());
        }
        let created = text(row, "current_created")?;
        let history_created = text(row, "history_created")?;
        crate::sql_time::native_time(&created)?;
        crate::sql_time::native_time(&history_created)?;
        Ok(Self { scope, database: id("database_id")?, current, history, created, history_created, at, period, unit })
    }
    pub(in crate::tables::native_tests) fn same_owner(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.database == other.database
            && self.current == other.current
            && self.history == other.history
            && self.created == other.created
            && self.history_created == other.history_created
    }
}
impl Fixture<'_> {
    pub(in crate::tables::native_tests) fn temporal_accounting(&self) -> serde_json::Value {
        serde_json::json!({"control":self.control_accounting(),"source_capture_unsettled":self.source_capture_unsettled,"only_previously_absent_nonce_objects_claimed":self.temporal.claimed})
    }
    pub(in crate::tables::native_tests) fn temporal_names(&self) -> [String; 2] {
        [self.name("temporal"), self.name("history")]
    }
    pub(in crate::tables::native_tests) fn temporal_witness(&self) -> &Witness {
        self.temporal.witness.as_ref().unwrap()
    }
    pub(in crate::tables::native_tests) fn temporal_oracle(&self, layout: RetentionLayout) -> String {
        let [current, history] = self.temporal_names();
        let retention = if layout == RetentionLayout::Available {
            format!(
                "(SELECT history_retention_period FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{current}')) AS configured_period,(SELECT history_retention_period_unit FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{current}')) AS configured_unit"
            )
        } else {
            "CAST(NULL AS int) AS configured_period,CAST(NULL AS int) AS configured_unit".into()
        };
        format!(
            "SELECT /* culprit fixture temporal oracle */ DB_ID() AS database_id,DB_NAME() AS native_database,@@SPID AS session_id, CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) AS machine,CONVERT(int,SERVERPROPERTY('EngineEdition')) AS edition,CONVERT(int,SERVERPROPERTY('ProductMajorVersion')) AS major,CONVERT(nvarchar(40),SYSUTCDATETIME(),127)+N'Z' AS observed_at,CONVERT(varchar(40),i.sqlserver_start_time,126) AS process_started_without_offset,CONVERT(nvarchar(36),d.database_guid) AS database_guid,CONVERT(nvarchar(36),d.recovery_fork_guid) AS recovery_fork_guid,CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),'Updateability')) AS updateability,HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visible,(SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.tables') AND name IN(N'history_retention_period',N'history_retention_period_unit',N'history_retention_period_unit_desc')) AS retention_columns,OBJECT_ID(N'dbo.{current}',N'U') AS current_id,OBJECT_ID(N'dbo.{history}',N'U') AS history_id,(SELECT temporal_type FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{current}')) AS current_type,(SELECT temporal_type FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{history}')) AS history_type,(SELECT history_table_id FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{current}')) AS current_history_id,(SELECT history_table_id FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{history}')) AS history_history_id,(SELECT CONVERT(varchar(40),create_date,126) FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{current}')) AS current_created,(SELECT CONVERT(varchar(40),create_date,126) FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{history}')) AS history_created,{retention} FROM sys.dm_os_sys_info i CROSS JOIN sys.database_recovery_status d WHERE d.database_id=DB_ID()"
        )
    }
    pub(in crate::tables::native_tests) async fn initialize_temporal(&mut self, layout: RetentionLayout) -> Result<()> {
        let [current, history] = self.temporal_names();
        let viewer = self.name("viewer");
        let oracle = self.temporal_oracle(layout);
        let ids=self.control("culprit-temporal-setup",async |commands,_|{
            let before=commands.row(&format!("SELECT /* culprit fixture temporal admission */ CONVERT(int,SERVERPROPERTY('ProductMajorVersion')) AS major,CONVERT(int,SERVERPROPERTY('EngineEdition')) AS edition,OBJECT_ID(N'dbo.{current}',N'U') AS current_id,OBJECT_ID(N'dbo.{history}',N'U') AS history_id,(SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.tables') AND name IN(N'history_retention_period',N'history_retention_period_unit',N'history_retention_period_unit_desc')) AS retention_columns")).await;
            if !matches!(integer(&before,"edition")?,2|3|4) || !layout.accepts(integer(&before,"major")?,integer(&before,"retention_columns")?){return Err(culprit_core::Error::new(culprit_core::ErrorCode::Unsupported,"This owned selector requires its exact supported/unsupported native temporal layout; no temporal positive was run."))}
            assert_eq!(optional_integer(&before,"current_id")?,None,"no existing current table may be claimed");assert_eq!(optional_integer(&before,"history_id")?,None,"no existing history table may be claimed");
            commands.claim_temporal();
            let retention=if layout==RetentionLayout::Available{",HISTORY_RETENTION_PERIOD=6 MONTHS"}else{""};
            commands.batch(&format!("CREATE TABLE dbo.[{current}](id int NOT NULL CONSTRAINT [{current}_key] PRIMARY KEY,valid_from datetime2 GENERATED ALWAYS AS ROW START NOT NULL,valid_to datetime2 GENERATED ALWAYS AS ROW END NOT NULL,PERIOD FOR SYSTEM_TIME(valid_from,valid_to)) WITH(SYSTEM_VERSIONING=ON(HISTORY_TABLE=dbo.[{history}]{retention})); GRANT SELECT ON OBJECT::dbo.[{current}] TO [{viewer}]; GRANT SELECT ON OBJECT::dbo.[{history}] TO [{viewer}]; DENY VIEW DEFINITION TO [{viewer}];")).await;
            Witness::decode(&commands.row(&oracle).await,layout)
        }).await?;
        assert_eq!(
            ids.database,
            self.ids.as_ref().unwrap().database,
            "the temporal pair belongs to the already admitted original database"
        );
        assert_eq!(ids.scope[1], self.owned.database, "native current database matches the admitted nonce route");
        self.temporal.witness = Some(ids);
        Ok(())
    }
    pub(in crate::tables::native_tests) async fn infinite_temporal_retention(&mut self) -> Result<()> {
        let current = self.temporal_names()[0].clone();
        let oracle = self.temporal_oracle(RetentionLayout::Available);
        let original = self.temporal_witness().clone();
        let changed = self
            .control("culprit-temporal-retention", async |commands, _| {
                commands
                    .batch(&format!(
                        "ALTER TABLE dbo.[{current}] SET(SYSTEM_VERSIONING=ON(HISTORY_RETENTION_PERIOD=INFINITE));"
                    ))
                    .await;
                Witness::decode(&commands.row(&oracle).await, RetentionLayout::Available)
            })
            .await?;
        assert!(
            original.same_owner(&changed),
            "a configuration change cannot replace the original table/database/process"
        );
        self.temporal.witness = Some(changed);
        Ok(())
    }
    pub(super) fn temporal_cleanup_sql(&self) -> String {
        if !self.temporal.claimed {
            return String::new();
        }
        let [current, history] = self.temporal_names();
        let guard = match &self.temporal.witness {
            None => String::new(),
            Some(witness) => format!(
                "IF OBJECT_ID(N'dbo.{current}',N'U') IS NOT NULL AND (OBJECT_ID(N'dbo.{current}',N'U')<>{} OR (SELECT CONVERT(varchar(40),create_date,126) FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{current}',N'U'))<>{}) THROW 51000,N'Owned temporal current identity changed',1; IF OBJECT_ID(N'dbo.{history}',N'U') IS NOT NULL AND (OBJECT_ID(N'dbo.{history}',N'U')<>{} OR (SELECT CONVERT(varchar(40),create_date,126) FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{history}',N'U'))<>{}) THROW 51000,N'Owned temporal history identity changed',1; ",
                witness.current,
                crate::catalog_visibility::sql_literal(&witness.created),
                witness.history,
                crate::catalog_visibility::sql_literal(&witness.history_created)
            ),
        };
        format!(
            "{guard}IF OBJECT_ID(N'dbo.{current}',N'U') IS NOT NULL BEGIN IF EXISTS(SELECT 1 FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{current}') AND temporal_type=2) ALTER TABLE dbo.[{current}] SET(SYSTEM_VERSIONING=OFF); DROP TABLE dbo.[{current}]; END; IF OBJECT_ID(N'dbo.{history}',N'U') IS NOT NULL DROP TABLE dbo.[{history}]; "
        )
    }
    pub(super) fn assert_temporal_identity(&self, table: &MssqlTableSnapshot) {
        let native = self.temporal_witness();
        assert_eq!(table.identity.database_id, native.database);
        let current = table.name == self.temporal_names()[0];
        assert_eq!(table.identity.object_id, if current { native.current } else { native.history });
        assert_eq!(
            crate::sql_time::native_time(&table.identity.created_without_offset).unwrap(),
            crate::sql_time::native_time(if current { &native.created } else { &native.history_created }).unwrap()
        );
        assert!(table.is_valid());
    }
    pub(in crate::tables::native_tests) fn begin_table_read(&mut self) {
        assert!(!self.source_capture_unsettled);
        self.source_capture_unsettled = true;
    }
    pub(in crate::tables::native_tests) fn table_read_settled(&mut self) {
        assert!(self.source_capture_unsettled);
        self.source_capture_unsettled = false;
    }
}
