//! Native declaration/binding qualification is confined to a nonce-named disposable loopback database.
use super::*;
use crate::{
    cached_definitions,
    conn::{MsClient, Pool, SessionKind},
    read::{NativeRead, ReadResult, ReadSession},
    test_support::{batch, config},
};
use culprit_core::{
    Plan,
    plan_facts::RelationalInputKind,
    telemetry::{CollectionBudget, DefinitionPayload, PayloadUnavailable, SourceShutdown},
};
use futures_util::FutureExt;
use std::{panic::AssertUnwindSafe, sync::Arc, time::Instant};

const STATEMENT: &str = "SELECT id AS tvp_original_header_probe FROM @Rows";
const TVP: &str = "@Rows dbo.RowsType READONLY,@secret nvarchar(100)=N'header-default-canary' AS";
const LOCAL: &str = "@unused int=1 AS DECLARE @Rows dbo.RowsType;INSERT @Rows VALUES(1);";

async fn execute(client: &mut MsClient, tvp: bool) {
    batch(
        client,
        if tvp {
            "DECLARE @input dbo.RowsType;INSERT @input VALUES(1),(2);EXEC dbo.header_probe @Rows=@input"
        } else {
            "EXEC dbo.header_probe"
        },
    )
    .await;
}

async fn definition(reader: &mut ReadSession, object: i64) -> culprit_core::telemetry::MssqlCachedDefinition {
    let limits = cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
    let rows = reader
        .query(&cached_definitions::select_sql(&limits), vec![Parameter::Integer(501)], 501)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let row = rows
        .iter()
        .find(|row| {
            crate::read::optional_signed_integer(row, "module_object_id").unwrap() == Some(object)
                && row.str("statement_text").is_some_and(|text| text.contains("tvp_original_header_probe"))
        })
        .expect("owned module statement must be in the bounded native cache projection");
    let value = cached_definitions::decode(row, &limits).unwrap();
    let mut bindings = Bindings::default();
    assert!(bindings.admit(&value.context).unwrap());
    let verified = bindings.verify(reader).await.unwrap();
    assert!(bindings.all_verified(&verified));
    value
}

struct AlterAfterPayload {
    io: ReadSession,
    application: MsClient,
    changed: bool,
}
#[async_trait::async_trait]
impl NativeRead for AlterAfterPayload {
    async fn query(&mut self, query: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        let rows = self.io.query(query, parameters, limit).await?;
        if query.contains("sys.dm_exec_text_query_plan") && !self.changed {
            assert!(
                rows.iter().any(|row| row
                    .str("statement_text")
                    .is_some_and(|text| text.contains("tvp_original_header_probe")))
            );
            self.changed = true;
            batch(&mut self.application, &format!("ALTER PROCEDURE dbo.header_probe {LOCAL} {STATEMENT}")).await;
            execute(&mut self.application, false).await;
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

fn qualified_input(plan: &Plan) -> bool {
    fn visit(node: &culprit_core::PlanNode) -> bool {
        node.facts
            .relational_input
            .is_some_and(|input| input.qualified_kind() == RelationalInputKind::TableValuedParameter)
            || node.children.iter().any(visit)
    }
    visit(&plan.root)
}

fn plan_work(plans: &[String]) {
    for xml in plans {
        culprit_core::xml::read(xml, |document| {
            let document = document.unwrap();
            for node in document.descendants().filter(|node| node.is_element()) {
                if matches!(node.tag_name().name(), "QueryTimeStats" | "RunTimeCountersPerThread" | "MemoryGrantInfo") {
                    eprintln!(
                        "bounded attribute lookup {} {:?}",
                        node.tag_name().name(),
                        node.attributes().map(|a| (a.name(), a.value())).collect::<Vec<_>>()
                    );
                }
            }
        });
    }
}

#[tokio::test]
#[ignore = "requires an owned culprit-smoke-mssql loopback fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT / PASSWORD"]
async fn original_module_headers_bind_tvp_kind_and_withhold_reused_handle_enrichment_after_alter() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    let machine = crate::conn::query_one(
        &mut admin,
        "SELECT /* culprit */ CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) AS machine",
    )
    .await
    .unwrap()
    .string("machine");
    let nonce =
        machine.strip_prefix("culprit-smoke-mssql-").expect("only the owned disposable fixture may mutate a module");
    assert_eq!(nonce.len(), 24);
    assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let database = format!("culprit_tvp_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    batch(&mut admin, &format!("CREATE DATABASE [{database}] COLLATE Latin1_General_100_CS_AS")).await;
    let proof=AssertUnwindSafe(async {
        let pool=Arc::new(Pool::new(config(port,&database,&password,"culprit"),None));
        let mut application=crate::test_support::connect(&config(port,&database,&password,"tvp-header-fixture"),SessionKind::User).await.unwrap();
        batch(&mut application,"CREATE TYPE dbo.RowsType AS TABLE(id int PRIMARY KEY)").await;
        batch(&mut application,&format!("CREATE PROCEDURE dbo.header_probe {TVP} {STATEMENT}")).await;
        execute(&mut application,true).await;
        let object=crate::conn::query_one(&mut application,"SELECT /* culprit */ OBJECT_ID(N'dbo.header_probe') AS object_id").await.unwrap();
        let object=crate::read::signed_integer(&object,"object_id").unwrap();
        let mut reader=ReadSession::application(pool.clone());
        let original=definition(&mut reader,object).await;
        let metadata=original.module_parameters.as_ref().expect("the original READONLY header must qualify TVP kind");
        assert!(metadata.input(&original.context,"@Rows").is_some());
        assert!(metadata.input(&original.context,"@rows").is_none(),"case-sensitive native spelling must not be guessed");
        assert!(!serde_json::to_string(metadata).unwrap().contains("header-default-canary"));
        let hash=original.context.query_hash.clone().unwrap();
        let details=crate::cached_statements::details(&mut reader,std::slice::from_ref(&hash),1,&ReadSession::application_budget()).await.unwrap();
        let detail=&details[&hash];
        let plan=crate::plan_xml::parse_cached_statement(detail.plan.text().unwrap(),&detail.context,detail.module_parameters.as_ref(),detail.statement.text()).unwrap();
        assert!(qualified_input(&plan),"the actual selected native plan must carry the proven READONLY input kind");
        let mut bindings=Bindings::default();
        bindings.admit(&original.context).unwrap();
        let started=Instant::now();
        let plans=crate::test_support::assert_no_execution_grant(&mut application,sql::VERIFY,vec![bindings.parameter()]).await;
        eprintln!("native original-header lookup:1 exact handle, elapsed {:?}; plan={};sql={};statement={:?}; parameters={:?}",
            started.elapsed(),original.context.identity.plan_handle,original.context.identity.sql_handle,original.context.identity.statement_start_bytes,metadata.table_parameters);
        plan_work(&plans);
        assert_eq!(reader.close().await,SourceShutdown::Quiescent);

        let mut racy=AlterAfterPayload{io:ReadSession::application(pool.clone()),application,changed:false};
        let details=crate::cached_statements::details(&mut racy,std::slice::from_ref(&hash),1,&ReadSession::application_budget()).await.unwrap();
        assert!(racy.changed);
        let withheld=&details[&hash];
        assert!(withheld.statement.text().unwrap().contains("tvp_original_header_probe"));
        assert!(matches!(withheld.plan,crate::query_store::payload::Payload::Unavailable(PayloadUnavailable::SourceUnavailable)));
        assert!(withheld.module_parameters.is_none());
        assert_eq!(racy.close().await,SourceShutdown::Quiescent);
        let mut fresh=ReadSession::application(pool.clone());
        let local=definition(&mut fresh,object).await;
        assert_ne!(original.context.identity.sql_handle,local.context.identity.sql_handle);
        let local_metadata=local.module_parameters.as_ref().expect("a complete header without READONLY remains a known declaration");
        assert!(local_metadata.table_parameters.is_empty(),"the same table type used as a local must never become a TVP");
        assert!(matches!(local.statement.payload,DefinitionPayload::Available{..}));
        assert_eq!(fresh.close().await,SourceShutdown::Quiescent);
        eprintln!("same-body ALTER source proof: original plan={} SQL={}; current plan={} SQL={}; unchanged object_id={object}; original statement kept, unsafe plan/type withheld",
            original.context.identity.plan_handle,original.context.identity.sql_handle,local.context.identity.plan_handle,local.context.identity.sql_handle);
        batch(&mut racy.application,"DROP PROCEDURE dbo.header_probe").await;
        batch(&mut racy.application,&format!("CREATE PROCEDURE dbo.header_probe {TVP} {STATEMENT}")).await;
        execute(&mut racy.application,true).await;
        let recreated_object=crate::conn::query_one(&mut racy.application,"SELECT /* culprit */ OBJECT_ID(N'dbo.header_probe') AS object_id").await.unwrap();
        let recreated_object=crate::read::signed_integer(&recreated_object,"object_id").unwrap();
        assert_ne!(object,recreated_object);
        let mut fresh=ReadSession::application(pool);
        let recreated=definition(&mut fresh,recreated_object).await;
        assert!(recreated.module_parameters.as_ref().unwrap().input(&recreated.context,"@Rows").is_some());
        assert_eq!(fresh.close().await,SourceShutdown::Quiescent);
    }).catch_unwind().await;
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}]"),
    )
    .await;
    if let Err(error) = proof {
        std::panic::resume_unwind(error)
    }
}
