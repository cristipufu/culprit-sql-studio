//! Actual catalog shape only: two-row indexed view, same reader/SQL/decoder, no wait, benefit or cost claim.
use super::*;
const CONTROL_COMMANDS: usize = 5;
const INDEXED_VIEW_OPTIONS: &str = "SET ANSI_NULLS ON;SET ANSI_PADDING ON;SET ANSI_WARNINGS ON;SET ARITHABORT ON;SET CONCAT_NULL_YIELDS_NULL ON;SET QUOTED_IDENTIFIER ON;SET NUMERIC_ROUNDABORT OFF;";
#[tokio::test]
#[ignore = "requires the positively nonce-owned SQL2022+ fixture; shared catalog SQL/decoder proof, not Azure factory or production-cost qualification"]
async fn owned_indexed_view_keeps_native_parent_and_scan_stream_rows_out_of_table_indexes() {
    let owned = Box::pin(OwnedSourceFixture::open("culprit-indexed-view-fixture")).await.unwrap();
    let suffix = format!("{}_{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let mut fixture = Fixture {
        owned: &owned,
        table: format!("culprit_iv_base_{suffix}"),
        user: String::new(),
        owns: false,
        control_available: true,
    };
    let view = format!("culprit_iv_{suffix}");
    let mut owns_view = false;
    let mut controls = 0;
    let result=AssertUnwindSafe(Box::pin(async {
        let sql=format!("SELECT /* culprit fixture */ CONVERT(int,SERVERPROPERTY('ProductMajorVersion')) AS major,OBJECT_ID(N'dbo.{}',N'U') AS table_id,OBJECT_ID(N'dbo.{view}',N'V') AS view_id",fixture.table);
        controls+=1;
        let row=Box::pin(fixture.control(async |actor|crate::conn::query_one(actor,&sql).await.unwrap())).await;
        assert!(integer(&row,"major").unwrap()>=16);
        for field in ["table_id","view_id"] {assert!(crate::read::optional_integer(&row,field).unwrap().is_none(),"existing objects cannot be claimed");}
        fixture.owns=true;
        controls+=1;
        Box::pin(fixture.command(&format!("{INDEXED_VIEW_OPTIONS}CREATE TABLE dbo.[{}](id int NOT NULL,payload int NOT NULL);INSERT dbo.[{}] VALUES(1,10),(2,20);IF @@ROWCOUNT<>2 THROW 51000,'owned indexed-view insert quota incomplete',1;",fixture.table,fixture.table))).await;
        owns_view=true;
        controls+=1;
        Box::pin(fixture.command(&format!("{INDEXED_VIEW_OPTIONS}EXEC sys.sp_executesql N'CREATE VIEW dbo.[{view}] WITH SCHEMABINDING AS SELECT id,COUNT_BIG(*) AS native_count FROM dbo.[{}] GROUP BY id';",fixture.table))).await;
        controls+=1;
        Box::pin(fixture.command(&format!("{INDEXED_VIEW_OPTIONS}CREATE UNIQUE CLUSTERED INDEX cx_owned ON dbo.[{view}](id) WITH(MAXDOP=1);CREATE NONCLUSTERED INDEX ix_owned ON dbo.[{view}](native_count) WITH(MAXDOP=1);"))).await;
        let scan=Box::pin(read_views(&mut fixture,&view,false)).await;
        let stream=Box::pin(read_views(&mut fixture,&view,true)).await;
        assert_eq!(scan,stream,"the existing application and streamed catalog share native facts");
        assert_eq!(scan.len(),2,"both native indexes are required, no skipped positive");
        assert_eq!(scan[0].index_id,1);assert!(scan[0].unique);assert_eq!(scan[1].index_id,2);
        assert_eq!(scan[0].parent_object,scan[1].parent_object);
        for index in &scan {
            assert_eq!(index.parent_kind,MssqlIndexParentKind::IndexedView);assert_eq!(index.parent_memory_optimized,None);
            assert_eq!(index.parent_name,view);assert_eq!(index.usage,MssqlIndexUsage::NotCollected);
            let configuration=index.configuration.as_ref().unwrap();assert_eq!(configuration.parent_kind,index.parent_kind);
            assert_eq!(configuration.parent_object,index.parent_object);assert_eq!(configuration.partitions.len(),1);
        }
    })).catch_unwind().await;
    if owned.pool.source_shutdown() != SourceShutdown::Quiescent {
        fixture.control_available = false;
    }
    let cleanup = AssertUnwindSafe(Box::pin(async {
        if fixture.control_available && fixture.owns {
            controls += 1;
            let drop_view = if owns_view {
                format!("IF OBJECT_ID(N'dbo.{view}',N'V') IS NOT NULL DROP VIEW dbo.[{view}];")
            } else {
                String::new()
            };
            Box::pin(fixture.command(&format!(
                "{drop_view}IF OBJECT_ID(N'dbo.{}',N'U') IS NOT NULL DROP TABLE dbo.[{}];",
                fixture.table, fixture.table
            )))
            .await;
            fixture.owns = false;
        }
        // An uncertain original control performs no replacement SQL; the nonce supervisor destroys its DB.
        assert!(controls <= CONTROL_COMMANDS, "setup and cleanup work have their own finite paid envelope");
    }))
    .catch_unwind()
    .await;
    if let Err(panic) = result {
        resume_unwind(panic);
    }
    cleanup.expect("original control owner cleanup");
}
async fn read_views(fixture: &mut Fixture<'_>, view: &str, stream: bool) -> Vec<MssqlIndexSnapshot> {
    let pool = Arc::new(Pool::new(
        config(fixture.owned.port, &fixture.owned.database, fixture.owned.password(), "culprit-indexed-view-source"),
        None,
    ));
    let mut reader = Box::new(ReadSession::application(pool.clone()));
    let result = AssertUnwindSafe(Box::pin(async {
        let layout = Layout { with_dmvs: false, expression_limit: MAX_CATALOG_EXPRESSION_BYTES };
        let indexes = if stream {
            reader
                .begin_stream(
                    &select_sql(&layout),
                    vec![Parameter::Integer((APPLICATION_INDEXES + 1) as i64)],
                    APPLICATION_INDEXES + 1,
                )
                .await
                .map_err(ReadError::into_error)?;
            let mut indexes = Vec::new();
            let mut native_rows = 0;
            loop {
                let batch = reader.next_batch().await.map_err(ReadError::into_error)?;
                native_rows += batch.rows.len();
                assert!(native_rows <= APPLICATION_INDEXES, "a native prefix cannot masquerade as complete");
                for row in batch.rows {
                    let index = decode(&row, &layout)?.snapshot;
                    if index.parent_name == view {
                        indexes.push(index);
                    }
                }
                if batch.complete {
                    break;
                }
            }
            indexes
        } else {
            let read = application(reader.as_mut(), false).await?;
            assert!(read.parent_prefix_complete);
            assert!(!read.with_usage);
            assert!(
                !read.table_indexes.iter().any(|index| index.table == view),
                "a view cannot enter table-only advice"
            );
            read.indexed_views.into_iter().filter(|index| index.parent_name == view).collect()
        };
        assert_eq!(reader.queries(), 1, "the population uses one existing native dispatch");
        Ok::<_, culprit_core::Error>(indexes)
    }))
    .catch_unwind()
    .await;
    let shutdown = Box::pin(reader.close()).await;
    let metrics = reader.read_metrics();
    if shutdown != SourceShutdown::Quiescent {
        fixture.control_available = false;
    }
    drop(reader);
    drop(pool);
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert!(!metrics.unwrap().has_unconfirmed_work());
    match result {
        Err(panic) => resume_unwind(panic),
        Ok(result) => result.expect("actual native indexed-view catalog"),
    }
}

#[test]
fn real_indexed_view_read_phase_keeps_native_work_off_the_poll_stack() {
    fn bounded<A, F: std::future::Future>(_: impl FnOnce(A) -> F) {
        assert!(std::mem::size_of::<F>() <= 4096, "native read phase must retain large state on the heap");
        assert!(
            std::mem::size_of::<F::Output>() <= 4096,
            "native response is not returned inline through panic wrappers"
        );
    }
    // The actual concrete phase type is inferred without constructing a connection or invoking the closure.
    bounded(|(fixture, view): (&'static mut Fixture<'static>, &'static str)| read_views(fixture, view, false));
}
