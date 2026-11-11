//! Full scan qualification against an owned database on disposable loopback SQL Server.
use super::*;
use crate::{
    conn::{MsClient, Pool, SessionKind},
    test_support::{batch, config},
};
use culprit_core::Provider;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn full_scan_uses_bounded_readers_and_retains_schema_and_workload() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let master = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let mut admin = master.connect(SessionKind::User).await.unwrap();
    let database = format!("culprit_scan_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis());
    batch(&mut admin,&format!("CREATE DATABASE [{database}]; ALTER DATABASE [{database}] SET QUERY_STORE=ON (OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL);")).await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let mut app = pool.connect(SessionKind::User).await.unwrap();
    batch(&mut app, "ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS=ON").await;
    batch(&mut app,"CREATE TABLE dbo.scan_fixture(id int PRIMARY KEY,payload varchar(200) NOT NULL); INSERT dbo.scan_fixture VALUES(1,'a'),(2,'b');").await;
    crate::test_support::query_store_ready(&mut app).await;
    batch(&mut app, "SELECT SUM(id) AS scan_workload_canary FROM dbo.scan_fixture; EXEC sys.sp_query_store_flush_db;")
        .await;
    seed_catalog(&mut app).await;
    let provider = MssqlProvider {
        endpoint_host: "127.0.0.1".into(),
        endpoint_port: port,
        environment: culprit_core::Environment::Development,
        pool: pool.clone(),
    };
    let result = snapshot(&provider, "fixture", &SnapshotOptions { plans: 0, top_queries: 50, ..Default::default() })
        .await
        .unwrap();
    assert_catalog_snapshot(&result);
    verify_recommendation_links(&pool, &result).await;
    verify_column_definitions_and_restricted_statistics(port, &database, &password).await;
    // Discovery reads native interval metadata without executing an application statement or inventing a cadence.
    verify_closed_interval_discovery(&pool).await;
    verify_partition_accounting(&pool).await;
    verify_object_metadata(&pool).await;
    verify_trigger_metadata(&pool, &result).await;
    verify_partition_boundaries(&pool, &result).await;
    verify_cached_definitions(&pool).await;
    assert_eq!(provider.source_shutdown(), culprit_core::telemetry::SourceShutdown::Quiescent);
    drop(app);
    drop(provider);
    drop(pool);
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{database}]"),
    )
    .await;
}

async fn seed_catalog(app: &mut MsClient) {
    batch(app,"DECLARE @first datetime2(7)=CONVERT(datetime2(7),'2026-11-10T00:00:00.1234567',126),@second datetime2(7)=CONVERT(datetime2(7),'2026-11-11T00:00:00.1234567',126); CREATE PARTITION FUNCTION culprit_date_pf(datetime2(7)) AS RANGE RIGHT FOR VALUES(@first,@second); CREATE PARTITION SCHEME culprit_date_ps AS PARTITION culprit_date_pf ALL TO([PRIMARY]); CREATE TABLE dbo.partition_date_fixture(id int,at datetime2(7)) ON culprit_date_ps(at); CREATE INDEX ix_partition_date_fixture ON dbo.partition_date_fixture(at) ON culprit_date_ps(at); INSERT dbo.partition_date_fixture VALUES(1,NULL),(2,'2035-01-01');").await;
    batch(app,"CREATE PARTITION FUNCTION culprit_left_pf(int) AS RANGE LEFT FOR VALUES(NULL,10,20); CREATE PARTITION SCHEME culprit_left_ps AS PARTITION culprit_left_pf ALL TO([PRIMARY]); CREATE TABLE dbo.partition_left_fixture(id int) ON culprit_left_ps(id);").await;
    batch(app,"CREATE PARTITION FUNCTION culprit_empty_pf(int) AS RANGE RIGHT FOR VALUES(1); ALTER PARTITION FUNCTION culprit_empty_pf() MERGE RANGE(1); CREATE PARTITION SCHEME culprit_empty_ps AS PARTITION culprit_empty_pf ALL TO([PRIMARY]); CREATE TABLE dbo.partition_empty_fixture(id int) ON culprit_empty_ps(id);").await;
    batch(
        app,
        "CREATE TRIGGER dbo.[audit:fixture] ON dbo.scan_fixture AFTER INSERT,UPDATE AS BEGIN SET NOCOUNT ON; END",
    )
    .await;
    batch(
        app,
        "CREATE TRIGGER dbo.disabled_trigger_fixture ON dbo.scan_fixture AFTER DELETE AS BEGIN SET NOCOUNT ON; END",
    )
    .await;
    batch(app, "DISABLE TRIGGER dbo.disabled_trigger_fixture ON dbo.scan_fixture").await;
    batch(app, "CREATE VIEW dbo.trigger_view_fixture AS SELECT id FROM dbo.scan_fixture").await;
    batch(app,"CREATE TRIGGER dbo.view_trigger_fixture ON dbo.trigger_view_fixture INSTEAD OF INSERT AS BEGIN SET NOCOUNT ON; END").await;
    batch(app, "CREATE TABLE dbo.storage_fixture(id int PRIMARY KEY,a varchar(6000),b varchar(6000),large_value varchar(max));
     INSERT dbo.storage_fixture VALUES(1,REPLICATE('a',5000),REPLICATE('b',5000),REPLICATE(CAST('c' AS varchar(max)),12000)),(2,REPLICATE('d',5000),REPLICATE('e',5000),REPLICATE(CAST('f' AS varchar(max)),12000));
     CREATE INDEX ix_storage_fixture ON dbo.storage_fixture(id) INCLUDE(a);
     CREATE TABLE dbo.columnstore_fixture(id int,amount int);
     INSERT dbo.columnstore_fixture VALUES(1,100),(2,200);
     CREATE CLUSTERED COLUMNSTORE INDEX cc_fixture ON dbo.columnstore_fixture;").await;
    batch(app, "CREATE TABLE dbo.fk_parent_fixture([first] int,[other] int,PRIMARY KEY([first],[other]));
     CREATE TABLE dbo.fk_child_fixture([second] int,[parent\u{1f}key] int,
       CONSTRAINT FK_fixture_child_parent FOREIGN KEY([parent\u{1f}key],[second]) REFERENCES dbo.fk_parent_fixture([first],[other]) ON DELETE CASCADE);
     ALTER TABLE dbo.fk_child_fixture NOCHECK CONSTRAINT FK_fixture_child_parent;").await;
    batch(app,"CREATE INDEX ix_unusual_column_fixture ON dbo.fk_child_fixture([parent\u{1f}key] DESC) INCLUDE(second);
     CREATE TABLE dbo.spatial_fixture(id int PRIMARY KEY,shape geometry);
     CREATE SPATIAL INDEX ix_spatial_fixture ON dbo.spatial_fixture(shape) USING GEOMETRY_GRID WITH(BOUNDING_BOX=(-10,-10,10,10));").await;
    batch(app,"CREATE TABLE dbo.column_definition_fixture(id int IDENTITY PRIMARY KEY,owner nvarchar(40) NOT NULL DEFAULT N'fixture-secret',amount decimal(12,2),adjusted AS (amount+1) PERSISTED,CONSTRAINT ck_amount_fixture CHECK(amount>=0));
     INSERT dbo.column_definition_fixture(amount) VALUES(1),(2);
     CREATE STATISTICS stats_amount_fixture ON dbo.column_definition_fixture(amount);
     UPDATE STATISTICS dbo.column_definition_fixture WITH FULLSCAN;
     ALTER TABLE dbo.column_definition_fixture NOCHECK CONSTRAINT ck_amount_fixture;
     CREATE USER catalog_viewer WITHOUT LOGIN; GRANT VIEW DEFINITION TO catalog_viewer;").await;
    batch(app,"CREATE TABLE dbo.missing_index_fixture(id int IDENTITY PRIMARY KEY,[tenant]]id] int,payload varchar(200));
     INSERT dbo.missing_index_fixture([tenant]]id],payload) SELECT TOP(10000) ROW_NUMBER() OVER(ORDER BY (SELECT NULL))%100,REPLICATE('x',200) FROM sys.all_objects a CROSS JOIN sys.all_objects b;
     SELECT mf.payload,f.payload FROM dbo.missing_index_fixture mf JOIN dbo.scan_fixture f ON f.id=mf.[tenant]]id] WHERE mf.[tenant]]id]=1 ORDER BY mf.payload,f.payload OPTION(MAXDOP 1);
     EXEC sys.sp_query_store_flush_db;").await;
}

async fn verify_partition_boundaries(pool: &Arc<Pool>, snapshot: &culprit_core::Snapshot) {
    use crate::{partitions, read::Parameter};
    let native = pool
        .observe(|reader| {
            Box::pin(async move {
                let rows = reader
                    .query(
                        &partitions::select_sql(
                            partitions::Selection::AllIndexes,
                            culprit_core::telemetry::MAX_CATALOG_EXPRESSION_BYTES,
                        ),
                        vec![Parameter::Integer(101)],
                        101,
                    )
                    .await
                    .map_err(crate::read::ReadError::into_error)?;
                rows.iter()
                    .map(|row| partitions::decode(row, culprit_core::telemetry::MAX_CATALOG_EXPRESSION_BYTES))
                    .collect::<culprit_core::Result<Vec<_>>>()
            })
        })
        .await
        .unwrap();
    assert!(native.iter().any(|bound| bound.definition.index_id > 1));
    let null = native.iter().find(|bound| bound.definition.value_is_null == Some(true)).unwrap();
    assert!(null.definition.boundary_id.is_some());
    assert!(null.definition.native_value_sha256.is_none());
    let date = native
        .iter()
        .find(|bound| {
            bound.definition.table_name == "partition_date_fixture" && bound.definition.value_is_null == Some(false)
        })
        .unwrap();
    assert!(date.definition.boundary_on_right);
    assert!(date.definition.native_value_sha256.is_some());
    assert_eq!(date.definition.parameter_type, "datetime2");
    assert!(date.raw_display_value.as_deref().unwrap().contains("2026-11-10"));
    let empty = native.iter().find(|bound| bound.definition.table_name == "partition_empty_fixture").unwrap();
    assert_eq!(empty.definition.fanout, 1);
    assert!(empty.definition.boundary_id.is_none());
    let table = snapshot.tables.iter().find(|table| table.name == "partition_left_fixture").unwrap();
    assert!(
        table.storage.as_ref().unwrap().partition_bounds().iter().all(|bound| bound.contains("LESS THAN OR EQUAL"))
    );
}

async fn verify_trigger_metadata(pool: &Arc<Pool>, snapshot: &culprit_core::Snapshot) {
    use crate::{read::Parameter, triggers};
    let native = pool
        .observe(|reader| {
            Box::pin(async move {
                let rows = reader
                    .query(&triggers::select_sql(triggers::Selection::AllDml), vec![Parameter::Integer(101)], 101)
                    .await
                    .map_err(crate::read::ReadError::into_error)?;
                rows.iter().map(triggers::decode).collect::<culprit_core::Result<Vec<_>>>()
            })
        })
        .await
        .unwrap();
    assert_eq!(native.len(), 3);
    let disabled = native.iter().find(|trigger| trigger.name == "disabled_trigger_fixture").unwrap();
    assert!(disabled.disabled);
    let view = native.iter().find(|trigger| trigger.name == "view_trigger_fixture").unwrap();
    assert!(view.instead_of);
    assert_eq!(view.parent_type, "V");
    assert!(native.iter().all(|trigger| trigger.event_metadata_complete()));
    let table = snapshot.tables.iter().find(|table| table.name == "scan_fixture").unwrap();
    assert_eq!(table.storage.as_ref().unwrap().triggers(), [("audit:fixture", "IU")]);
}

fn assert_catalog_snapshot(result: &Snapshot) {
    assert!(result.tables.iter().any(|table| table.schema == "dbo" && table.name == "scan_fixture"));
    assert!(result.indexes.iter().any(|index| index.schema == "dbo" && index.table == "scan_fixture"));
    let key = result.foreign_keys.iter().find(|key| key.name == "FK_fixture_child_parent").unwrap();
    assert_eq!(key.columns, vec!["parent\u{1f}key", "second"]);
    assert_eq!(key.ref_columns, vec!["first", "other"]);
    assert_eq!(key.delete_rule.as_deref(), Some("CASCADE"));
    assert!(result.constraints.iter().any(|issue| issue.name == key.name && issue.disabled));
    assert!(result.constraints.iter().any(|issue| issue.name == "ck_amount_fixture"
        && issue.disabled
        && issue.kind == culprit_core::ConstraintKind::Check));
    let unusual = result.indexes.iter().find(|index| index.name == "ix_unusual_column_fixture").unwrap();
    assert_eq!(unusual.columns, vec!["parent\u{1f}key DESC"]);
    assert_eq!(unusual.include, vec!["second"]);
    let spatial = result.indexes.iter().find(|index| index.name == "ix_spatial_fixture").unwrap();
    assert!(spatial.reads.is_none() && spatial.writes.is_none(), "unsupported usage cannot become zero");
    let column = result
        .columns
        .iter()
        .find(|column| column.table == "column_definition_fixture" && column.name == "owner")
        .unwrap();
    assert_eq!(column.data_type, "nvarchar(40)");
    assert_eq!(column.default.as_deref(), Some("(N'fixture-secret')"));
    assert!(
        result
            .columns
            .iter()
            .any(|column| column.table == "column_definition_fixture" && column.name == "adjusted" && column.generated)
    );
    assert!(result.statistics.iter().any(|stats| stats.name == "stats_amount_fixture" && stats.rows == Some(2.0)));
    assert!(result.workload.as_ref().unwrap().queries.iter().any(|query| query.text.contains("scan_workload_canary")));
    for source in [
        crate::sources::TABLES.name,
        crate::sources::INDEXES.name,
        crate::sources::COLUMNS.name,
        crate::sources::QUERY_STORE.name,
        crate::sources::QUERY_FAMILIES.name,
        crate::sources::QUERY_COMPILATION.name,
        crate::sources::QUERY_COSTS.name,
    ] {
        let outcome = result.sources.iter().find(|outcome| outcome.source == source).unwrap();
        assert!(
            matches!(outcome.state, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty),
            "{outcome:?}"
        );
    }
}

async fn verify_recommendation_links(pool: &Arc<Pool>, result: &Snapshot) {
    let native_recommendations=pool.observe(|reader|Box::pin(async move {
        reader.query("SELECT mid.equality_columns,mid.inequality_columns,migs.user_seeks,migs.user_scans FROM sys.dm_db_missing_index_details mid JOIN sys.dm_db_missing_index_groups mig ON mig.index_handle=mid.index_handle JOIN sys.dm_db_missing_index_group_stats migs ON migs.group_handle=mig.index_group_handle WHERE mid.database_id=DB_ID() AND mid.object_id=OBJECT_ID('dbo.missing_index_fixture') OPTION(MAXDOP 1)",vec![],10).await.map_err(crate::read::ReadError::into_error)
    })).await.unwrap();
    let recommendation =
        result.missing_indexes.iter().find(|index| index.table == "missing_index_fixture").unwrap_or_else(|| {
            panic!(
                "Missing-index fixture was not returned; native: {native_recommendations:?}; outcomes: {:?}",
                result
                    .sources
                    .iter()
                    .filter(|source| source.source == crate::sources::MISSING_INDEXES.name)
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(recommendation.equality, vec!["tenant]id"]);
    assert!(!recommendation.query_hashes.is_empty());
}

async fn verify_column_definitions_and_restricted_statistics(port: u16, database: &str, password: &str) {
    let permission_pool = Arc::new(Pool::new(config(port, database, password, "culprit"), None));
    permission_pool
        .observe(|reader| {
            Box::pin(async move {
                use crate::read::Parameter;
                use culprit_core::telemetry::*;
                let rows = reader
                    .query(
                        &crate::columns::select_sql(MAX_CATALOG_EXPRESSION_BYTES),
                        vec![Parameter::Integer(1000)],
                        1000,
                    )
                    .await
                    .map_err(crate::read::ReadError::into_error)?;
                let column = rows
                    .iter()
                    .map(|row| crate::columns::decode(row, MAX_CATALOG_EXPRESSION_BYTES))
                    .collect::<culprit_core::Result<Vec<_>>>()?
                    .into_iter()
                    .find(|column| {
                        column.definition.table_name == "column_definition_fixture" && column.definition.name == "owner"
                    })
                    .unwrap();
                assert!(!serde_json::to_string(&column.definition).unwrap().contains("fixture-secret"));
                let checks = reader
                    .query(
                        &crate::check_constraints::select_sql(MAX_CATALOG_EXPRESSION_BYTES),
                        vec![Parameter::Integer(1000)],
                        1000,
                    )
                    .await
                    .map_err(crate::read::ReadError::into_error)?;
                let checks = checks
                    .iter()
                    .map(|row| crate::check_constraints::decode(row, MAX_CATALOG_EXPRESSION_BYTES))
                    .collect::<culprit_core::Result<Vec<_>>>()?;
                let check = checks.iter().find(|check| check.name == "ck_amount_fixture").unwrap();
                assert!(check.disabled && !check.trusted);
                assert_eq!(check.parent_table, "column_definition_fixture");

                reader
                    .query("EXECUTE AS USER='catalog_viewer'", vec![], 1)
                    .await
                    .map_err(crate::read::ReadError::into_error)?;
                let statistics = crate::statistics::application(reader).await?;
                assert!(
                    !statistics.properties_complete,
                    "catalog visibility cannot establish readable statistics properties"
                );
                assert!(
                    statistics.statistics.iter().any(|stat| stat.name == "stats_amount_fixture" && stat.rows.is_none())
                );
                Ok(())
            })
        })
        .await
        .unwrap();
    drop(permission_pool);
}

async fn verify_closed_interval_discovery(pool: &Arc<Pool>) {
    let until = chrono::Utc::now();
    let discovery = culprit_core::telemetry::IntervalDiscoveryRequest {
        from: (until - chrono::Duration::days(1)).to_rfc3339(),
        until: until.to_rfc3339(),
        cursor: None,
        limit: 8,
        budget: Default::default(),
    };
    pool.observe(|reader| {
        Box::pin(async move {
            use crate::query_store::intervals;
            let (at, metadata_from) = intervals::clock(reader).await?;
            assert!(metadata_from.is_some());
            let page = intervals::page(reader, &discovery, &at, None).await?;
            let after = intervals::Position { end: discovery.from.clone(), id: 1 };
            let resumed = intervals::page(reader, &discovery, &at, Some(&after)).await?;
            assert_eq!(resumed, page);
            assert!(page.iter().all(|interval| {
                chrono::DateTime::parse_from_rfc3339(&interval.until).unwrap()
                    <= chrono::DateTime::parse_from_rfc3339(&at).unwrap()
            }));
            Ok(())
        })
    })
    .await
    .unwrap();
}

async fn verify_partition_accounting(pool: &Arc<Pool>) {
    pool.observe(|reader| {
        Box::pin(async move {
            let native = crate::tables::application(reader, true).await?;
            let catalog = crate::tables::application(reader, false).await?;
            assert!(native.complete_scope && catalog.complete_scope);
            let full = native.tables.iter().find(|table| table.name == "storage_fixture").unwrap();
            let fallback = catalog.tables.iter().find(|table| table.name == "storage_fixture").unwrap();
            assert_eq!(full.rows, Some(2.0));
            assert_eq!(fallback.rows, Some(2.0), "allocation kinds cannot multiply partition row counts");
            assert!(full.lob_bytes.is_some_and(|bytes| bytes > 0.0));
            assert!(full.overflow_bytes.is_some_and(|bytes| bytes > 0.0));
            assert_eq!(fallback.total_bytes, full.total_bytes);
            assert_eq!(fallback.data_bytes, full.data_bytes);
            assert_eq!(fallback.index_bytes, full.index_bytes);
            assert_eq!(fallback.in_row_bytes, full.in_row_bytes);
            assert_eq!(fallback.lob_bytes, full.lob_bytes);
            assert_eq!(fallback.overflow_bytes, full.overflow_bytes);
            let (indexes, _) = crate::indexes::application(reader, true).await?;
            let (catalog_indexes, _) = crate::indexes::application(reader, false).await?;
            for index in indexes.iter().filter(|index| index.table == "storage_fixture") {
                let fallback = catalog_indexes
                    .iter()
                    .find(|candidate| candidate.table == index.table && candidate.name == index.name)
                    .unwrap();
                assert!(index.size_bytes.is_some_and(|bytes| bytes > 0.0));
                assert_eq!(fallback.size_bytes, index.size_bytes);
            }
            assert!(
                catalog_indexes.iter().find(|index| index.table == "columnstore_fixture").unwrap().size_bytes.is_none()
            );
            let columnstore = catalog.tables.iter().find(|table| table.name == "columnstore_fixture").unwrap();
            assert_eq!(columnstore.rows, Some(2.0));
            assert!(columnstore.total_bytes.is_none());
            assert!(columnstore.data_bytes.is_none());
            Ok(())
        })
    })
    .await
    .unwrap();
}

async fn verify_object_metadata(pool: &Arc<Pool>) {
    let mut reader = crate::read::ReadSession::application(pool.clone());
    let rows = reader
        .query(&crate::recorded::select_sql(false), vec![crate::read::Parameter::Integer(10_001)], 10_001)
        .await
        .map_err(crate::read::ReadError::into_error)
        .unwrap();
    let objects = rows.iter().map(crate::recorded::decode).collect::<Result<Vec<_>>>().unwrap();
    let object = objects.iter().find(|object| object.name == "scan_fixture").unwrap();
    assert_eq!(object.native_type, "U");
    assert!(!object.modified_without_offset.ends_with('Z'));
    let events = crate::recorded::changes(&mut reader, "2000-01-01T00:00:00Z").await.unwrap();
    assert!(events.iter().any(|event| event.title.contains("scan_fixture")));
    assert_eq!(reader.close().await, culprit_core::telemetry::SourceShutdown::Quiescent);
}

async fn verify_cached_definitions(pool: &Arc<Pool>) {
    use crate::{
        cached_definitions,
        read::{Parameter, ReadError, ReadSession},
        retained_definitions,
    };
    use culprit_core::telemetry::*;
    let mut reader = ReadSession::application(pool.clone());
    let limits = cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
    let rows = reader
        .query(&cached_definitions::select_sql(&limits), vec![Parameter::Integer(501)], 501)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let definitions =
        rows.iter().map(|row| cached_definitions::decode(row, &limits)).collect::<Result<Vec<_>>>().unwrap();
    let canary=definitions.iter().find(|definition|matches!(&definition.statement.payload,DefinitionPayload::Available {text,..} if text.contains("scan_workload_canary"))).unwrap();
    assert!(canary.estimated_plan.payload.is_available());
    assert!(canary.estimated_plan.native_sha256.is_some());
    let layout = retained_definitions::layout(&mut reader, &CollectionBudget::default()).await.unwrap().unwrap();
    assert_eq!(layout.database_setting, Some(true));
    let rows = reader
        .query(&retained_definitions::select_sql(&layout), vec![Parameter::Integer(501)], 501)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let retained =
        rows.iter().map(|row| retained_definitions::decode(row, &layout)).collect::<Result<Vec<_>>>().unwrap();
    assert!(retained.iter().any(|definition| definition.context.identity == canary.context.identity
        && definition.batch_plan.payload.is_available()));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
