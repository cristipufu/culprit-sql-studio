//! Native PSP acceptance is confined to a nonce-owned loopback SQL Server fixture.
use super::*;
use crate::{
    conn::{Pool, SessionKind},
    query_store::runtime::PlanSummary,
    read::{NativeRead, Parameter, ReadResult, ReadSession},
    test_support::{batch, config},
    value::Rec,
};
use culprit_core::telemetry::SourceShutdown;
use futures_util::FutureExt;
use std::{
    collections::BTreeSet,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};

struct RetainOriginalSummary {
    io: ReadSession,
    rows: Arc<Mutex<Vec<PlanSummary>>>,
}

fn same_duration(observed_ms: f64, original_us: f64) {
    let expected_ms = crate::query_store::runtime::milliseconds(original_us);
    assert!(
        (observed_ms - expected_ms).abs() <= 1e-9_f64.max(expected_ms.abs() * 1e-12),
        "{observed_ms} ms differs from the original {original_us} us"
    );
}

#[async_trait::async_trait]
impl NativeRead for RetainOriginalSummary {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        let rows = self.io.query(sql, parameters, limit).await?;
        if sql.contains(" AS known_executions") {
            *self.rows.lock().unwrap() =
                rows.iter().map(PlanSummary::decode).collect::<culprit_core::Result<_>>().unwrap();
        }
        Ok(rows)
    }
    fn queries(&self) -> u32 {
        self.io.queries()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

#[tokio::test]
#[ignore = "requires an owned culprit-smoke-mssql loopback SQL2022 fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT / PASSWORD"]
async fn native_child_only_psp_relationships_preserve_original_runtime_population() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    let platform = crate::conn::query_one(&mut admin,
        "SELECT /* culprit */ CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) AS machine,CAST(SERVERPROPERTY('ProductMajorVersion') AS int) AS major").await.unwrap();
    let host = platform.string("machine");
    let nonce =
        host.strip_prefix("culprit-smoke-mssql-").expect("only the owned smoke fixture may execute this workload");
    assert!(nonce.len() == 24 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(crate::read::integer(&platform, "major").unwrap(), 16, "acceptance is qualified on SQL Server2022");
    let database = format!("culprit_psp_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    let proof = AssertUnwindSafe(async {
        batch(&mut admin,&format!("ALTER DATABASE [{database}] SET COMPATIBILITY_LEVEL=160;ALTER DATABASE [{database}] SET QUERY_STORE=ON(OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL,WAIT_STATS_CAPTURE_MODE=OFF)")).await;
        let pool = Arc::new(Pool::new(config(port,&database,&password,"culprit"),None));
        let mut application = crate::test_support::connect(&config(port,&database,&password,"parameter-variant-fixture"),SessionKind::User).await.unwrap();
        crate::test_support::query_store_ready(&mut application).await;
        batch(&mut application,"ALTER DATABASE SCOPED CONFIGURATION SET PARAMETER_SENSITIVE_PLAN_OPTIMIZATION=ON").await;
        // Finite owned fixture: exactly1048576 rows, a16-byte payload, one seed and six serial calls.
        // The smaller histogram is deliberately not assumed eligible for native PSP.
        batch(&mut application,"CREATE TABLE dbo.parameter_variant_rows(id int NOT NULL PRIMARY KEY,bucket int NOT NULL,payload char(16) NOT NULL)").await;
        batch(&mut application,r#"CREATE PROCEDURE dbo.parameter_variant_seed AS BEGIN
WITH eight(n) AS(SELECT n FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7))v(n)),four(n) AS(SELECT n FROM(VALUES(0),(1),(2),(3))v(n))
INSERT dbo.parameter_variant_rows(id,bucket,payload)
SELECT 1+a.n+8*b.n+64*c.n+512*d.n+4096*e.n+16384*f.n+131072*g.n,CASE WHEN a.n+8*b.n+64*c.n+512*d.n+4096*e.n+16384*f.n+131072*g.n<786432 THEN 1 ELSE 2+(a.n+8*b.n+64*c.n+512*d.n+4096*e.n+16384*f.n+131072*g.n)%262144 END,
 CONVERT(char(16),CONCAT('native-',a.n,'-',b.n,'-',c.n,'-',d.n,'-',e.n))
FROM eight a CROSS JOIN eight b CROSS JOIN eight c CROSS JOIN eight d CROSS JOIN four e CROSS JOIN eight f CROSS JOIN eight g;
CREATE INDEX parameter_variant_bucket ON dbo.parameter_variant_rows(bucket);
UPDATE STATISTICS dbo.parameter_variant_rows WITH FULLSCAN;END"#).await;
        let seed_object = crate::conn::query_one(&mut application,"SELECT /* culprit */ OBJECT_ID(N'dbo.parameter_variant_seed') AS object_id").await.unwrap();
        let seed_object_id = crate::read::integer(&seed_object,"object_id").unwrap().to_string();
        let seed_started = std::time::Instant::now();
        batch(&mut application,"EXEC dbo.parameter_variant_seed").await;
        let seed_client_ms = seed_started.elapsed().as_secs_f64()*1000.0;
        let storage = crate::conn::query_one(&mut application,"SELECT /* culprit */ SUM(CASE WHEN index_id IN(0,1) THEN row_count ELSE 0 END) AS rows,SUM(used_page_count) AS used_pages FROM sys.dm_db_partition_stats WHERE object_id=OBJECT_ID(N'dbo.parameter_variant_rows')").await.unwrap();
        assert_eq!(crate::read::integer(&storage,"rows").unwrap(),1_048_576);
        eprintln!("owned PSP fixture native rows={} table/index used8KiB pages={} seed client_ms={seed_client_ms}",crate::read::integer(&storage,"rows").unwrap(),crate::read::integer(&storage,"used_pages").unwrap());
        for file in crate::conn::query(&mut application,"SELECT /* culprit */ name,type_desc,CONVERT(bigint,size)*8 AS allocated_kib FROM sys.database_files").await.unwrap() {
            eprintln!("owned PSP fixture native file={} type={} allocated_kib={}",file.string("name"),file.string("type_desc"),crate::read::integer(&file,"allocated_kib").unwrap());
        }
        batch(&mut application,"CREATE PROCEDURE dbo.parameter_variant_probe @bucket int AS SELECT SUM(CONVERT(bigint,LEN(payload))) AS native_payload_length FROM dbo.parameter_variant_rows WHERE bucket=@bucket").await;
        let object = crate::conn::query_one(&mut application,"SELECT /* culprit */ OBJECT_ID(N'dbo.parameter_variant_probe') AS object_id").await.unwrap();
        let object_id = crate::read::integer(&object,"object_id").unwrap().to_string();
        for bucket in [1,2,1,2,1,2] { batch(&mut application,&format!("EXEC dbo.parameter_variant_probe @bucket={bucket}")).await; }
        batch(&mut application,"EXEC sys.sp_query_store_flush_db").await;
        let original = Arc::new(Mutex::new(Vec::new()));
        let native = RetainOriginalSummary{io:ReadSession::application(pool.clone()),rows:original.clone()};
        let mut reader = crate::query_store::Reader::new(native);
        let result = from_query_store(&mut reader,&SnapshotOptions{window_minutes:60,plans:10,top_queries:200,..Default::default()}).await;
        assert_eq!(reader.close().await,SourceShutdown::Quiescent);
        drop(reader);
        let result = result.unwrap();
        let rows = original.lock().unwrap();
        let seed_rows = rows.iter().filter(|row|row.identity.qualifiers.get("object_id")==Some(&seed_object_id)).collect::<Vec<_>>();
        assert!(!seed_rows.is_empty(),"native QueryStore must retain the uniquely identified seed module costs");
        for row in seed_rows {
            eprintln!("owned PSP seed native query={} plan={} executions={} duration_us={} cpu_us={} logical_reads={} physical_reads={}",row.identity.query_id,row.plan_id,row.moments.executions,row.moments.duration_us,row.moments.cpu_us,row.moments.logical_reads,row.moments.physical_reads);
        }
        let children = result.workload.queries.iter().filter(|query|query.variant_family.as_ref().is_some_and(|family|family.parent.qualifiers.get("object_id")==Some(&object_id))).collect::<Vec<_>>();
        assert!(children.len()>=2,"owned skewed inputs must actually generate distinct native PSP child queries; families={:?}",result.workload.queries.iter().filter_map(|query|query.variant_family.as_ref()).collect::<Vec<_>>());
        let mut dispatchers=BTreeSet::new();
        let mut child_runs=0.0;
        for child in children {
            let family=child.variant_family.as_ref().unwrap();
            assert!(family.is_valid());
            assert_eq!(family.variant.query_id,child.id);
            assert_eq!(result.query_identities.get(&child.id),Some(&family.variant));
            assert!(!rows.iter().any(|row|row.identity.query_id==family.parent.query_id),"dispatcher must genuinely have no native runtime rows");
            assert!(!result.workload.queries.iter().any(|query|query.id==family.parent.query_id),"metadata must not synthesize a parent runtime population");
            let native=rows.iter().filter(|row|row.identity==family.variant).collect::<Vec<_>>();
            assert!(!native.is_empty());
            assert_eq!(child.executions,native.iter().map(|row|row.moments.executions).sum::<u64>() as f64);
            same_duration(child.total_time_ms,native.iter().map(|row|row.moments.duration_us).sum());
            assert_eq!(child.plans.len(),native.len());
            for plan in &child.plans {
                let original=native.iter().find(|row|row.plan_id==plan.id).unwrap();
                assert_eq!(plan.executions,original.moments.executions as f64);
                same_duration(plan.total_time_ms,original.moments.duration_us);
            }
            child_runs+=child.executions;
            dispatchers.insert(family.dispatcher.plan_id.clone());
            eprintln!("owned native PSP child={} parent={} dispatcher={} original plans={} runs={} total_ms={}",child.id,family.parent.query_id,family.dispatcher.plan_id,native.len(),child.executions,child.total_time_ms);
        }
        assert_eq!(child_runs,6.0,"only the six original application executions are counted");
        assert_eq!(dispatchers.len(),1,"the fixture must exercise one native dispatcher family");
        assert_eq!(result.workload.total_executions,rows.iter().map(|row|row.moments.executions).sum::<u64>() as f64);
        same_duration(result.workload.total_time_ms,rows.iter().map(|row|row.moments.duration_us).sum());
        drop(application);drop(pool);
    }).catch_unwind().await;
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}]"),
    )
    .await;
    if let Err(error) = proof {
        std::panic::resume_unwind(error);
    }
}
