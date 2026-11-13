//! Cache objects and statement rows have different cardinalities. Only owned loopback fixtures execute SQL here.
use super::*;
use crate::{
    conn::{Pool, SessionKind},
    read::ReadSession,
    test_support::{batch, config},
};
use culprit_core::telemetry::{CollectionBudget, SourceShutdown};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const TABLES: [&str; 7] = ["exclusive", "reused", "prepared", "left", "right", "same", "partial"];
const VARIANTS: u32 = 12;

fn statement(table: &str, value: u32) -> String {
    // A join and aggregate keep literal variants as native Adhoc entries rather than a trivial auto-parameterized
    // lookup. The fixture verifies the native object types instead of assuming that this syntax established them.
    format!(
        "SELECT SUM(a.id+b.id) FROM dbo.footprint_{table} a JOIN dbo.footprint_lookup b ON a.id=b.id WHERE a.id={value} OPTION(MAXDOP 1)"
    )
}

async fn seed(app: &mut crate::conn::MsClient) {
    batch(app, "CREATE TABLE dbo.footprint_lookup(id int NOT NULL PRIMARY KEY);INSERT dbo.footprint_lookup VALUES(1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11),(12)").await;
    for table in TABLES {
        batch(app, &format!("SELECT id INTO dbo.footprint_{table} FROM dbo.footprint_lookup")).await;
    }
    for value in 1..=VARIANTS {
        batch(app, &statement("exclusive", value)).await;
        let reused = statement("reused", value);
        batch(app, &reused).await;
        batch(app, &reused).await;
        let prepared = statement("prepared", value).replace("WHERE a.id=", "WHERE b.id>=@floor AND a.id=");
        batch(app, &format!("EXEC sys.sp_executesql N'{prepared}',N'@floor int',@floor=0")).await;
        batch(app, &format!("{};{}", statement("left", value), statement("right", value))).await;
        batch(app, &format!("{};{}", statement("same", value), statement("same", value + VARIANTS))).await;
        // The unexecuted statement is not necessarily present in query_stats. Native offsets must still prevent
        // the first completed statement from claiming the bytes of this entire batch.
        batch(app, &format!("{};IF 1=0 BEGIN {} END", statement("partial", value), statement("right", value))).await;
    }
}

struct NativeEntry {
    hash: String,
    handle: String,
    object_type: String,
    lookups: u64,
    bytes: u64,
    statement: String,
}

async fn entries(reader: &mut ReadSession) -> Vec<NativeEntry> {
    let sql = format!(
        "SELECT CONVERT(varchar(18),qs.query_hash,1) AS query_hash,CONVERT(varchar(130),qs.plan_handle,1) AS plan_handle,
 cp.objtype,cp.usecounts,cp.size_in_bytes,{} AS statement_text
 {} JOIN sys.dm_exec_cached_plans cp ON cp.plan_handle=qs.plan_handle
 WHERE {} AND st.text LIKE N'%FROM dbo.footprint_%' AND st.text NOT LIKE N'%sp_executesql%'
 AND st.text NOT LIKE N'%sys.dm_exec_%' OPTION(MAXDOP 1)",
        crate::cached_statements::statement_expression("qs", "st.text"),
        crate::cached_statements::relation(),
        crate::cached_statements::DATABASE_PREDICATE,
    );
    reader
        .query(&sql, vec![], 1001)
        .await
        .map_err(ReadError::into_error)
        .unwrap()
        .iter()
        .map(|row| NativeEntry {
            hash: text(row, "query_hash").unwrap(),
            handle: text(row, "plan_handle").unwrap(),
            object_type: text(row, "objtype").unwrap(),
            lookups: integer(row, "usecounts").unwrap() as u64,
            bytes: integer(row, "size_in_bytes").unwrap() as u64,
            statement: text(row, "statement_text").unwrap(),
        })
        .collect()
}

fn family<'a>(families: &'a [QueryFamily], entries: &[NativeEntry], table: &str) -> &'a QueryFamily {
    let marker = format!("FROM dbo.footprint_{table} ");
    let hashes = entries
        .iter()
        .filter(|entry| entry.statement.contains(&marker))
        .map(|entry| entry.hash.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(hashes.len(), 1, "the planted {table} variants must have one native query family");
    let hash = hashes.first().unwrap();
    families.iter().find(|family| family.key == *hash).unwrap_or_else(|| panic!("missing {table} family {hash}"))
}

async fn report_native_work(reader: &mut ReadSession, client_elapsed_ms: f64, returned_rows: usize) {
    // Separate native CPU/I/O from client elapsed time. This records this tiny owned fixture's work; it does not
    // establish a production-cardinality cost bound or replace qualification under concurrent load.
    let rows = reader
        .query(
            "SELECT SUM(qs.execution_count) AS executions,SUM(qs.total_worker_time) AS worker_us,
 SUM(qs.total_logical_reads) AS logical_reads,SUM(qs.total_physical_reads) AS physical_reads
 FROM sys.dm_exec_query_stats qs CROSS APPLY sys.dm_exec_sql_text(qs.sql_handle) st
 CROSS APPLY sys.dm_exec_plan_attributes(qs.plan_handle) pa
 WHERE pa.attribute='dbid' AND CAST(pa.value AS int)=DB_ID()
 AND st.text LIKE N'%WITH native_statements AS (%' AND st.text NOT LIKE N'%SUM(qs.total_worker_time)%'
 OPTION(MAXDOP 1)",
            vec![],
            2,
        )
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let [work] = rows.as_slice() else { panic!("native probe statistics") };
    assert_eq!(integer(work, "executions").unwrap(), 1);
    eprintln!(
        "cache-family owned fixture: client_ms={client_elapsed_ms:.3}, returned_rows={returned_rows}, native_worker_us={}, native_logical_reads={}, native_physical_reads={}",
        integer(work, "worker_us").unwrap(),
        integer(work, "logical_reads").unwrap(),
        integer(work, "physical_reads").unwrap(),
    );
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_family_bytes_count_exclusive_adhoc_objects_once_and_preserve_shared_batch_ownership_as_unknown() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let master = Arc::new(Pool::new(config(port, "master", &password, "culprit"), None));
    let mut admin = master.connect(SessionKind::User).await.unwrap();
    let database = format!("culprit_cache_bytes_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis());
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit-cache-fixture"), None));
    let mut app = pool.connect(SessionKind::User).await.unwrap();
    seed(&mut app).await;
    let mut reader = ReadSession::new(pool.clone(), CollectionBudget::default());
    let read = query_families(&mut reader, 60, None::<&mut crate::query_store::Reader<ReadSession>>).await.unwrap();
    assert!(read.cache_failure.is_none());
    assert_eq!(reader.queries(), 1, "family metadata and object accounting use one native statement");
    let metrics = reader.read_metrics().unwrap();
    assert_eq!(metrics.templates.len(), 1);
    let elapsed = metrics.templates[0].client_elapsed_ms;
    let native = entries(&mut reader).await;
    assert!(!native.is_empty());

    let exclusive = family(&read.families, &native, "exclusive");
    let objects = native
        .iter()
        .filter(|entry| entry.hash == exclusive.key)
        .map(|entry| {
            assert_eq!(entry.object_type, "Adhoc");
            assert_eq!(entry.lookups, 1);
            (entry.handle.as_str(), entry.bytes)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(objects.len(), VARIANTS as usize);
    let expected_bytes = objects.values().sum::<u64>();
    assert!(expected_bytes > 0);
    assert_eq!(exclusive.cached_bytes, Some(expected_bytes as f64));

    for (table, expected_type) in [("reused", "Adhoc"), ("prepared", "Prepared")] {
        let family = family(&read.families, &native, table);
        let entries = native.iter().filter(|entry| entry.hash == family.key).collect::<Vec<_>>();
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|entry| entry.object_type == expected_type && entry.bytes > 0));
        if table == "reused" {
            assert!(entries.iter().all(|entry| entry.lookups >= 2));
        }
        assert_eq!(family.cached_bytes, Some(0.0));
    }
    for table in ["left", "right", "same", "partial"] {
        assert_eq!(family(&read.families, &native, table).cached_bytes, None, "{table}");
    }
    let left = family(&read.families, &native, "left");
    let right = family(&read.families, &native, "right");
    let left_objects =
        native.iter().filter(|entry| entry.hash == left.key).map(|entry| &entry.handle).collect::<BTreeSet<_>>();
    let right_objects =
        native.iter().filter(|entry| entry.hash == right.key).map(|entry| &entry.handle).collect::<BTreeSet<_>>();
    assert_eq!(left_objects, right_objects, "two query families share the same native batch objects");
    assert_eq!(left_objects.len(), VARIANTS as usize);
    let same = family(&read.families, &native, "same");
    let same_rows = native.iter().filter(|entry| entry.hash == same.key).collect::<Vec<_>>();
    let same_objects = same_rows.iter().map(|entry| &entry.handle).collect::<BTreeSet<_>>();
    assert_eq!(same_rows.len(), same_objects.len() * 2, "even one family's batch bytes are not per-statement bytes");

    report_native_work(&mut reader, elapsed, read.families.len()).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    drop(reader);
    drop(app);
    drop(pool);
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}]"),
    )
    .await;
}
