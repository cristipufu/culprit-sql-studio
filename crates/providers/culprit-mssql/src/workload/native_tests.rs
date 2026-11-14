//! Only a nonce-named disposable loopback fixture may mutate Query Store for this interleaving proof.
use super::*;
use crate::{
    conn::{MsClient, Pool, SessionKind},
    query_store::{Reader, catalog_sql, definitions, runtime::PlanSummary, window_sql::FrozenWindow},
    read::{NativeRead, Parameter, ReadResult, ReadSession},
    test_support::{batch, config},
    value::Rec,
};
use culprit_core::telemetry::SourceShutdown;
use futures_util::FutureExt;
use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};

const APPLICATION_SQL: &str =
    "SELECT 10 / @divisor AS runtime_cohort_failure_probe FROM dbo.runtime_cohort_rows WHERE id=1";

fn native_membership(window: &FrozenWindow) -> String {
    let Parameter::Text(membership) = window.parameter() else { panic!("native membership is a bound XML parameter") };
    membership
}

async fn execute(client: &mut MsClient, fails: bool) {
    let divisor = if fails { 0 } else { 1 };
    batch(
        client,
        &format!(
            "BEGIN TRY EXEC dbo.runtime_cohort_failure_probe @divisor={divisor}; \
             END TRY BEGIN CATCH IF ERROR_NUMBER()<>8134 THROW; END CATCH"
        ),
    )
    .await;
}

#[derive(Default)]
struct CohortProof {
    original: Option<PlanSummary>,
    reset: Option<PlanSummary>,
    reset_window: Option<FrozenWindow>,
}

struct ResetBeforeDefinitions {
    io: ReadSession,
    application: MsClient,
    pool: Arc<Pool>,
    query_id: String,
    plan_id: String,
    reset: bool,
    proof: Arc<Mutex<CohortProof>>,
}

#[async_trait::async_trait]
impl NativeRead for ResetBeforeDefinitions {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        if sql == catalog_sql::QUERY_LENGTHS && !self.reset {
            self.reset = true;
            assert!(self.proof.lock().unwrap().original.is_some(), "the real summary must precede the reset");
            let plan_id = self.plan_id.parse::<u64>().unwrap();
            batch(&mut self.application, &format!("EXEC sys.sp_query_store_reset_exec_stats {plan_id}")).await;
            for _ in 0..4 {
                execute(&mut self.application, false).await;
            }
            batch(&mut self.application, "EXEC sys.sp_query_store_flush_db").await;
            // Independent fixture observation establishes what the source now contains. It is never returned to
            // the production reader in place of its already-retained sufficient statistics.
            let mut observer = Reader::open(self.pool.clone());
            let rows = observer.summary(60).await.unwrap();
            let reset = rows.into_iter().find(|row| row.identity.query_id == self.query_id).unwrap();
            let window = observer.window(60).await.unwrap();
            assert_eq!(observer.close().await, SourceShutdown::Quiescent);
            let mut proof = self.proof.lock().unwrap();
            proof.reset = Some(reset);
            proof.reset_window = Some(window);
        }
        let rows = self.io.query(sql, parameters, limit).await?;
        if sql.contains(" AS known_executions") {
            let original = rows
                .iter()
                .find(|row| row.str("query_id").as_deref() == Some(self.query_id.as_str()))
                .map(PlanSummary::decode)
                .transpose()
                .unwrap()
                .expect("the actual runtime summary must contain the owned statement");
            self.proof.lock().unwrap().original = Some(original);
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
#[ignore = "requires an owned culprit-smoke-mssql loopback fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT / PASSWORD"]
async fn native_reset_between_summary_and_definitions_preserves_original_failure_population() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    let host = crate::conn::query_one(
        &mut admin,
        "SELECT /* culprit */ CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) AS machine",
    )
    .await
    .unwrap()
    .string("machine");
    let nonce = host.strip_prefix("culprit-smoke-mssql-").expect("only the disposable smoke fixture is permitted");
    assert!(nonce.len() == 24 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let database = format!("culprit_cohort_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    let proof = AssertUnwindSafe(async {
        batch(&mut admin, &format!("ALTER DATABASE [{database}] SET COMPATIBILITY_LEVEL=120;ALTER DATABASE [{database}] SET QUERY_STORE=ON(OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL,WAIT_STATS_CAPTURE_MODE=OFF)")).await;
        let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
        let mut application = crate::test_support::connect(
            &config(port, &database, &password, "runtime-cohort-fixture"), SessionKind::User,
        ).await.unwrap();
        crate::test_support::query_store_ready(&mut application).await;
        batch(&mut application,"CREATE TABLE dbo.runtime_cohort_rows(id int PRIMARY KEY);INSERT dbo.runtime_cohort_rows VALUES(1)").await;
        batch(&mut application,&format!("CREATE PROCEDURE dbo.runtime_cohort_failure_probe @divisor int AS {APPLICATION_SQL}")).await;
        let object = crate::conn::query_one(&mut application,"SELECT /* culprit */ OBJECT_ID(N'dbo.runtime_cohort_failure_probe') AS object_id").await.unwrap();
        let object_id = crate::read::integer(&object,"object_id").unwrap().to_string();
        execute(&mut application,true).await;
        for _ in 0..3 {execute(&mut application,false).await;}
        batch(&mut application,"EXEC sys.sp_query_store_flush_db").await;
        let mut preflight = Reader::open(pool.clone());
        let rows = preflight.summary(60).await.unwrap();
        let candidates = rows.iter().filter(|row|row.identity.qualifiers.get("object_id")==Some(&object_id)).collect::<Vec<_>>();
        assert_eq!(candidates.len(),1,"the unique owned procedure must contribute exactly one native query/plan");
        let original = candidates[0];
        let definitions = preflight.definitions(std::slice::from_ref(&original.identity.query_id),definitions::Kind::Query,256*1024).await.unwrap();
        let definition = &definitions[0];
        assert_eq!(definition.query.as_ref(),Some(&original.identity));
        assert_eq!(definition.object_name.as_deref(),Some("dbo.runtime_cohort_failure_probe"));
        assert!(definition.payload.text().is_some(),"the owned native module statement must be readable");
        assert_eq!(original.moments.executions,4);
        assert_eq!(original.costs.failed,1,"the owned native exception must be recorded before reset");
        let original_window = preflight.window(60).await.unwrap();
        assert_eq!(preflight.close().await,SourceShutdown::Quiescent);
        let evidence = Arc::new(Mutex::new(CohortProof::default()));
        let native = ResetBeforeDefinitions{
            io:ReadSession::application(pool.clone()),application,pool:pool.clone(),
            query_id:definition.id.clone(),plan_id:original.plan_id.clone(),reset:false,proof:evidence.clone(),
        };
        let mut reader = Reader::new(native);
        let result = from_query_store(&mut reader,&SnapshotOptions{window_minutes:60,plans:0,top_queries:100,..Default::default()}).await;
        let production_window = if result.is_ok() {Some(reader.window(60).await.unwrap())} else {None};
        assert_eq!(reader.close().await,SourceShutdown::Quiescent);
        drop(reader);
        let result = result.unwrap();
        let evidence = evidence.lock().unwrap();
        let before = evidence.original.as_ref().expect("production summary was retained before reset");
        let after = evidence.reset.as_ref().expect("separate owned actor reset runtime statistics");
        assert_eq!(before.identity,after.identity);
        assert_eq!(before.plan_id,after.plan_id);
        assert_eq!(before.moments.executions,after.moments.executions);
        assert_eq!(after.moments.executions,4);
        assert_eq!(after.costs.failed,0);
        assert_ne!(before.first_seen,after.first_seen,"native execution cohort must actually change");
        assert_eq!(native_membership(&original_window),native_membership(evidence.reset_window.as_ref().unwrap()),"this proof requires the existing interval identity to remain unchanged");
        assert_eq!(native_membership(&production_window.unwrap()),native_membership(&original_window),"the actual producer must use that same native interval population");
        let query = result.workload.queries.iter().find(|query|query.id==definition.id).unwrap();
        assert_eq!(query.executions,4.0);
        assert_eq!(query.total_time_ms,crate::query_store::runtime::milliseconds(before.moments.duration_us));
        assert_eq!(query.costs.as_ref().unwrap().failed,Some(1.0),"later same-ID/count cohort must not replace the original exception numerator");
        assert_eq!(query.costs.as_ref().unwrap().log_bytes,before.costs.log_bytes);
        assert_eq!(result.query_identities.get(&definition.id),Some(&before.identity));
        eprintln!("owned native reset proof: query={} plan={} native count={} unchanged identity/interval; original failed={} reset failed={} model failed={:?}; original time={}us reset time={}us",definition.id,before.plan_id,before.moments.executions,before.costs.failed,after.costs.failed,query.costs.as_ref().unwrap().failed,before.moments.duration_us,after.moments.duration_us);
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
