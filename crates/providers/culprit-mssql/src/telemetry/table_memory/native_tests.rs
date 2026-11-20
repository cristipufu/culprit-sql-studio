//! The original nonce-owned fixture pays for its tiny XTP setup. Production collection stays SELECT-only.
use super::*;
use crate::{
    read::{Parameter, integer, text},
    test_support::{
        OwnedSourceFixture,
        owned_captures::{Captured, OwnedCaptures},
        owned_case::{finish_case, until_cancelled},
    },
};
use culprit_collector::capture::CollectionCancellation;
use std::time::Duration;
use tokio::time::Instant;

#[tokio::test]
#[ignore = "requires a fresh nonce-owned disposable boxed SQL2022+ fixture with XTP; creates one two-row SCHEMA_ONLY table and owned memory filegroup; no cost qualification"]
async fn owned_selected_table_memory_is_real_factory_data_and_old_object_identity_cannot_attach_after_recreation() {
    let fixture = OwnedSourceFixture::open("culprit").await.unwrap();
    assert!(std::env::var_os("CULPRIT_MSSQL_REPLICATION_EXPECT_ROLE").is_none());
    assert!(std::env::var_os("CULPRIT_MSSQL_REPLICATION_EXPECT_GROUP").is_none());
    let cancellation = CollectionCancellation::default();
    let budget = CollectionBudget {
        max_queries: 3,
        max_snapshot_rows: 1,
        page_rows: 1,
        page_bytes: 4096,
        total_bytes: 4096,
        statement_timeout_ms: 10_000,
        total_timeout_ms: 45_000,
        ..Default::default()
    };
    let mut command = ReadSession::new(fixture.pool.clone(), budget);
    let mut captures = OwnedCaptures::from_pool(fixture.pool.clone(), fixture.port).unwrap();
    let table = format!("culprit_table_memory_{}", std::process::id());
    let (expired,result)=finish_case(Instant::now()+Duration::from_secs(60),&cancellation,async{
        // Positive nonce admission above owns this new catalog/filegroup/container. No existing filegroup is reused.
        let setup=format!(r#"IF CONVERT(int,SERVERPROPERTY('ProductMajorVersion'))<16 OR ISNULL(CONVERT(int,SERVERPROPERTY('IsXTPSupported')),0)<>1
 THROW 51000,'owned XTP environment unsupported',1;
IF OBJECT_ID(N'dbo.{table}',N'U') IS NOT NULL OR EXISTS(SELECT 1 FROM sys.filegroups WHERE type=N'FX')
 THROW 51000,'owned XTP setup must be fresh',1;
ALTER DATABASE [{database}] ADD FILEGROUP [culprit_owned_table_memory] CONTAINS MEMORY_OPTIMIZED_DATA;
ALTER DATABASE [{database}] ADD FILE(NAME=N'{database}_table_memory',FILENAME=N'/var/opt/mssql/data/{database}_table_memory') TO FILEGROUP [culprit_owned_table_memory];
CREATE TABLE dbo.[{table}](id int NOT NULL PRIMARY KEY NONCLUSTERED,payload int NULL) WITH(MEMORY_OPTIMIZED=ON,DURABILITY=SCHEMA_ONLY);
INSERT dbo.[{table}] VALUES(1,10),(2,20);
{identity}"#,database=fixture.database,identity=identity_sql(&table));
        let target=command_target(&mut command,&cancellation,&setup).await?;
        let original=capture(&mut captures,&cancellation,&target).await?;
        let replacement_sql=format!("DROP TABLE dbo.[{table}]; CREATE TABLE dbo.[{table}](id int NOT NULL PRIMARY KEY NONCLUSTERED,payload int NULL) WITH(MEMORY_OPTIMIZED=ON,DURABILITY=SCHEMA_ONLY); INSERT dbo.[{table}] VALUES(1,10),(2,20); {}",identity_sql(&table));
        let replacement=command_target(&mut command,&cancellation,&replacement_sql).await?;
        if replacement==target{return Err(invalid_data());}
        let refused=captures.capture_with_cancellation(CollectionSelection::TableMemory{target:Box::new(target.clone())},&cancellation).await;
        if !matches!(refused,Err(ref error) if matches!(CollectionStop::from_error(error),CollectionStop::SourceChanged|CollectionStop::SourceUnavailable)){return Err(invalid_data());}
        if fixture.pool.source_shutdown()!=SourceShutdown::Quiescent{return Err(CollectionStop::CancellationUnconfirmed.error());}
        let later=capture(&mut captures,&cancellation,&replacement).await?;
        until_cancelled(&cancellation,async{command.query(&format!("DROP TABLE dbo.[{table}]"),vec![],1).await.map_err(ReadError::into_error)}).await?;
        captures.verify_retained(&original).await?;captures.verify_retained(&later).await?;
        Ok::<_,Error>((original,later))
    }).await;
    // Same command client is always retired before assertions, including cancellation and intermediate source failure.
    let shutdown = command.close().await;
    let metrics = command.read_metrics();
    let (original, later) =
        result.expect("real selected XTP observations and immutable original after object recreation");
    assert!(!expired);
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert_eq!(command.queries(), 3);
    let metrics = metrics.unwrap();
    assert!(!metrics.has_unconfirmed_work());
    for captured in [&original, &later] {
        let point = point(captured);
        assert!(point.allocated_table_kibibytes.is_some());
        assert!(point.used_table_kibibytes.is_some());
        assert!(point.allocated_indexes_kibibytes.is_some());
        assert!(point.used_indexes_kibibytes.is_some());
        assert!(point.allocated_table_kibibytes.as_deref().is_some_and(|value| value != "0"));
        assert_eq!(captured.manifest.source_queries, 11);
        assert_eq!(captured.manifest.status, CollectionStatus::Complete);
        assert_eq!(captured.manifest.request.budget.max_queries, 12);
        assert_eq!(
            crate::telemetry::recipes().into_iter().find(|recipe| recipe.id == "mssql.table-memory").unwrap().cost,
            RecipeCostQualification::Unqualified
        );
    }
    assert_ne!(point(&original).table, point(&later).table);
    if std::env::var_os("CULPRIT_NATIVE_COST_RECEIPT_DIR").is_some() {
        crate::test_support::save_cost_receipt(
            &crate::test_support::receipt_directory(),
            "selected-table-memory-originals",
            &serde_json::json!({
                "successful":true,
                "original_manifest":original.manifest,
                "original_records":original.records,
                "replacement_manifest":later.manifest,
                "replacement_records":later.records,
                "old_target_refused_after_recreation":true,
                "originals_reverified_after_table_drop":true,
                "command_queries":command.queries(),
                "command_metrics":metrics,
                "command_shutdown":"Quiescent",
                "source_setup_is_separately_reserved":true,
                "rejected_capture_native_query_count":null,
                "whole_case_cost_qualified":false,
                "native_cost_approved":false,
            }),
        )
        .unwrap();
    }
    // SQL2022 cannot remove the last XTP container. The same disposable supervisor destroys this owned artifact.
}
fn identity_sql(table: &str) -> String {
    format!(
        "SELECT /* culprit fixture exact owned table */ DB_ID() AS database_id,object_id,CONVERT(varchar(40),create_date,126) AS created_without_offset FROM sys.tables WHERE object_id=OBJECT_ID(N'dbo.{table}',N'U') AND is_memory_optimized=1"
    )
}
async fn command_target(
    command: &mut ReadSession,
    cancellation: &CollectionCancellation,
    sql: &str,
) -> Result<MssqlObjectIdentity> {
    let rows = until_cancelled(cancellation, async {
        command.query(sql, Vec::<Parameter>::new(), 1).await.map_err(ReadError::into_error)
    })
    .await?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    Ok(MssqlObjectIdentity {
        database_id: integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?,
        object_id: integer(row, "object_id")?.try_into().map_err(|_| invalid_data())?,
        created_without_offset: crate::sql_time::native_time(&text(row, "created_without_offset")?)?,
    })
}
async fn capture(
    captures: &mut OwnedCaptures,
    cancellation: &CollectionCancellation,
    target: &MssqlObjectIdentity,
) -> Result<Captured> {
    captures
        .capture_with_cancellation(CollectionSelection::TableMemory { target: Box::new(target.clone()) }, cancellation)
        .await
}
fn point(capture: &Captured) -> &MssqlTableMemory {
    let source = capture.source().unwrap();
    let SourceObservation::TableMemory { metadata, target, .. } = &source.observation else {
        panic!("original table memory header")
    };
    assert!(metadata.available());
    assert!(table_memory_context(source).is_ok());
    let [record] = capture.records.as_slice() else { panic!("one native selected table memory row") };
    let TelemetryRecord::Operational(observation) = &record.record else { panic!("native operational sample") };
    let OperationalFact::MssqlTableMemory(point) = &observation.fact else { panic!("native table memory gauges") };
    assert_eq!(&point.table, target.as_ref());
    assert!(point.fits_source(source));
    point
}
