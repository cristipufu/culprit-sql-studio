//! Native PSP acceptance is confined to a nonce-owned loopback SQL Server fixture.
use super::*;
use crate::{
    conn::{Pool, SessionKind},
    read::ReadSession,
    test_support::{batch, config},
};
use culprit_core::telemetry::SourceShutdown;
use futures_util::FutureExt;
use std::{collections::BTreeSet, panic::AssertUnwindSafe, sync::Arc};

use crate::test_support::{
    parameter_variants,
    recorded_runtime::{RecordedRuntime, same_duration},
};

#[tokio::test]
#[ignore = "requires an owned culprit-smoke-mssql loopback SQL2022 fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT / PASSWORD"]
async fn native_child_only_psp_relationships_preserve_original_runtime_population() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    crate::test_support::verify_owned_source(&mut admin).await.unwrap();
    let platform = crate::conn::query_one(
        &mut admin,
        "SELECT /* culprit fixture */ CAST(SERVERPROPERTY('ProductMajorVersion') AS int) AS major",
    )
    .await
    .unwrap();
    assert_eq!(crate::read::integer(&platform, "major").unwrap(), 16, "acceptance is qualified on SQL Server2022");
    let database = format!("culprit_psp_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    let proof = AssertUnwindSafe(async {
        batch(&mut admin,&format!("ALTER DATABASE [{database}] SET COMPATIBILITY_LEVEL=160;ALTER DATABASE [{database}] SET QUERY_STORE=ON(OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL,WAIT_STATS_CAPTURE_MODE=OFF)")).await;
        let pool = Arc::new(Pool::new(config(port,&database,&password,"culprit"),None));
        let mut application = crate::test_support::connect(&config(port,&database,&password,"parameter-variant-fixture"),SessionKind::User).await.unwrap();
        crate::test_support::query_store_ready(&mut application).await;
        batch(&mut application,"ALTER DATABASE SCOPED CONFIGURATION SET PARAMETER_SENSITIVE_PLAN_OPTIMIZATION=ON").await;
        let seed=parameter_variants::seed(&mut application,&culprit_collector::capture::CollectionCancellation::default()).await;
        let seed_object_id=seed.object_id;
        batch(&mut application,"CREATE PROCEDURE dbo.parameter_variant_probe @bucket int AS SELECT SUM(CONVERT(bigint,LEN(payload))) AS native_payload_length FROM dbo.parameter_variant_rows WHERE bucket=@bucket").await;
        let object = crate::conn::query_one(&mut application,"SELECT /* culprit */ OBJECT_ID(N'dbo.parameter_variant_probe') AS object_id").await.unwrap();
        let object_id = crate::read::integer(&object,"object_id").unwrap().to_string();
        for bucket in [1,2,1,2,1,2] { batch(&mut application,&format!("EXEC dbo.parameter_variant_probe @bucket={bucket}")).await; }
        batch(&mut application,"EXEC sys.sp_query_store_flush_db").await;
        let native=RecordedRuntime::new(ReadSession::application(pool.clone()));
        let original=native.rows();
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
