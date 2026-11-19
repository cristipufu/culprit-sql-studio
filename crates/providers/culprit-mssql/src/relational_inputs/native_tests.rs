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
const LOCAL: &str = "@unused int=1 AS DECLARE @Rows dbo.RowsType;INSERT @Rows(id) VALUES(1);";
const CURRENT_TYPE_DDL: &str = "CREATE TYPE dbo.RowsType AS TABLE(id int NOT NULL PRIMARY KEY,label nvarchar(16) COLLATE Latin1_General_100_BIN2 NULL)";
const CATALOG_IDENTITY_SQL: &str = "SELECT /* culprit */ OBJECT_ID(N'dbo.header_probe') AS object_id,TYPE_ID(N'dbo.RowsType') AS user_type_id,(SELECT type_table_object_id FROM sys.table_types WHERE user_type_id=TYPE_ID(N'dbo.RowsType')) AS type_object_id,CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at";
const CURRENT_CLOCK_SQL: &str = "SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at";

mod expressions;
mod fixture;
mod reuse;
mod scalar;

async fn execute(client: &mut MsClient, tvp: bool) {
    batch(
        client,
        if tvp {
            "DECLARE @input dbo.RowsType;INSERT @input(id) VALUES(1);EXEC dbo.header_probe @Rows=@input"
        } else {
            "EXEC dbo.header_probe"
        },
    )
    .await;
}

async fn definition(reader: &mut ReadSession, object: i64) -> culprit_core::telemetry::MssqlCachedDefinition {
    named_definition(reader, object, "tvp_original_header_probe").await
}

async fn named_definition(
    reader: &mut ReadSession,
    object: i64,
    marker: &str,
) -> culprit_core::telemetry::MssqlCachedDefinition {
    let rows = named_definitions(reader, object, marker).await;
    let [row] = rows.as_slice() else {
        panic!("exactly one native statement, not an arbitrary first same-module variant")
    };
    row.fact.clone()
}

/// Original SQL stays only in this owned test's stack. The production decoder discards literal values normally.
/// Every selected cache point uses the existing final plan-to-SQL binding verifier before an actual XML join.
struct NamedDefinition {
    fact: culprit_core::telemetry::MssqlCachedDefinition,
    original_statement: String,
}
async fn named_definitions(reader: &mut ReadSession, object: i64, marker: &str) -> Vec<NamedDefinition> {
    verified_definitions(reader, |row| {
        crate::read::optional_signed_integer(row, "module_object_id").unwrap() == Some(object)
            && row.str("statement_text").is_some_and(|text| text.contains(marker))
    })
    .await
}

async fn exact_definitions(
    reader: &mut impl NativeRead,
    expected: &culprit_core::telemetry::CachedStatementContext,
) -> Vec<NamedDefinition> {
    let key = expected.identity.key().unwrap();
    verified_definitions(reader, |row| {
        let current = crate::cached_statements::context(row).unwrap();
        current.database_id == expected.database_id && current.identity.key().unwrap() == key
    })
    .await
}

async fn verified_definitions(
    reader: &mut impl NativeRead,
    select: impl Fn(&crate::value::Rec) -> bool,
) -> Vec<NamedDefinition> {
    let limits = cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
    let rows = reader
        .query(&cached_definitions::select_sql(&limits), vec![Parameter::Integer(501)], 501)
        .await
        .map_err(ReadError::into_error)
        .unwrap();
    let mut selected = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut bindings = Bindings::default();
    assert!(rows.len() < 501, "a truncated native cache roster cannot prove an exact selected original");
    for row in rows.iter().filter(|row| select(row)) {
        let fact = cached_definitions::decode(row, &limits, None).unwrap();
        assert!(
            seen.insert(fact.context.identity.key().unwrap()),
            "duplicate native cache point is not another variant"
        );
        assert!(bindings.admit(&fact.context).unwrap());
        selected.push(NamedDefinition { fact, original_statement: crate::read::text(row, "statement_text").unwrap() });
    }
    assert!(!selected.is_empty(), "the exact native selection must remain inside the bounded cache projection");
    let verified = bindings.verify(reader).await.unwrap();
    assert!(bindings.all_verified(&verified));
    selected
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

fn assert_current_type_schema(
    current: Option<&culprit_core::telemetry::MssqlCurrentTypeSchema>,
    context: &culprit_core::telemetry::CachedStatementContext,
    original: &culprit_core::telemetry::MssqlModuleParameters,
    expected: &crate::value::Rec,
    until: &str,
) {
    let current = current.expect("the native current catalog projection must accompany the original TVP");
    assert_eq!(current.status, culprit_core::telemetry::MssqlCurrentTypeSchemaStatus::Available);
    assert!(current.is_valid_for(context, Some(original)));
    let from = crate::read::text(expected, "observed_at").unwrap();
    let from = chrono::DateTime::parse_from_rfc3339(&from).unwrap();
    let until = chrono::DateTime::parse_from_rfc3339(until).unwrap();
    let observed = chrono::DateTime::parse_from_rfc3339(&current.observed_at).unwrap();
    assert!(from <= observed && observed <= until, "current catalog clock must belong to this native read");
    assert_eq!(
        i64::from(current.module.as_ref().unwrap().identity.object_id),
        crate::read::signed_integer(expected, "object_id").unwrap()
    );
    assert_eq!(current.parameters.len(), 1);
    assert_eq!(current.parameters[0].name, "@Rows");
    assert_eq!(current.parameters[0].parameter_id, 1);
    let [data_type] = current.types.as_slice() else { panic!("one exact native table type") };
    assert_eq!(i64::from(data_type.user_type_id), crate::read::signed_integer(expected, "user_type_id").unwrap());
    assert_eq!(
        i64::from(data_type.backing_object.object_id),
        crate::read::signed_integer(expected, "type_object_id").unwrap()
    );
    assert_eq!(data_type.schema, "dbo");
    assert_eq!(data_type.name, "RowsType");
    assert!(!data_type.memory_optimized);
    assert_eq!(data_type.columns.len(), 2);
    let id = data_type.columns.iter().find(|column| column.name == "id").unwrap();
    assert_eq!(id.type_name, "int");
    assert_eq!(id.max_length, 4);
    assert!(!id.nullable);
    assert!(id.collation.is_none());
    let label = data_type.columns.iter().find(|column| column.name == "label").unwrap();
    assert_eq!(label.type_name, "nvarchar");
    assert_eq!(label.max_length, 32, "nvarchar(16) retains its native byte count");
    assert!(label.nullable);
    assert_eq!(label.collation.as_deref(), Some("Latin1_General_100_BIN2"));
    let primary = data_type.indexes.iter().find(|index| index.primary_key).unwrap();
    assert!(primary.unique);
    let [key] = primary.columns.as_slice() else { panic!("one declared native primary key column") };
    assert_eq!(key.column_id, id.column_id);
    assert_eq!(key.name, "id");
    assert_eq!(key.key_ordinal, 1);
    assert!(!key.included);
    assert!(!key.descending);
    assert!(!serde_json::to_string(current).unwrap().contains("header-default-canary"));
}

#[tokio::test]
#[ignore = "requires an owned culprit-smoke-mssql loopback fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT / PASSWORD"]
async fn original_module_headers_bind_tvp_kind_and_withhold_reused_handle_enrichment_after_alter() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    crate::test_support::verify_owned_source(&mut admin).await.unwrap();
    let database = format!("culprit_tvp_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    batch(&mut admin, &format!("CREATE DATABASE [{database}] COLLATE Latin1_General_100_CS_AS")).await;
    let proof=AssertUnwindSafe(async {
        let pool=Arc::new(Pool::new(config(port,&database,&password,"culprit"),None));
        let mut application=crate::test_support::connect(&config(port,&database,&password,"tvp-header-fixture"),SessionKind::User).await.unwrap();
        batch(&mut application,CURRENT_TYPE_DDL).await;
        batch(&mut application,&format!("CREATE PROCEDURE dbo.header_probe {TVP} {STATEMENT}")).await;
        execute(&mut application,true).await;
        let native_identity=crate::conn::query_one(&mut application,CATALOG_IDENTITY_SQL).await.unwrap();
        let object=crate::read::signed_integer(&native_identity,"object_id").unwrap();
        let mut reader=ReadSession::application(pool.clone());
        let original=definition(&mut reader,object).await;
        let metadata=original.module_parameters.as_ref().expect("the original READONLY header must qualify TVP kind");
        assert!(metadata.input(&original.context,"@Rows").is_some());
        assert!(metadata.input(&original.context,"@rows").is_none(),"case-sensitive native spelling must not be guessed");
        assert!(!serde_json::to_string(metadata).unwrap().contains("header-default-canary"));
        let hash=original.context.query_hash.clone().unwrap();
        let details=crate::cached_statements::details(&mut reader,std::slice::from_ref(&hash),1,&ReadSession::application_budget(), true).await.unwrap();
        let detail=&details[&hash];
        let until=crate::conn::query_one(&mut application,CURRENT_CLOCK_SQL).await.unwrap();
        let until=crate::read::text(&until,"observed_at").unwrap();
        assert_current_type_schema(original.current_type_schema.as_ref(),&original.context,metadata,&native_identity,&until);
        assert_current_type_schema(detail.current_type_schema.as_ref(),&detail.context,detail.module_parameters.as_ref().unwrap(),&native_identity,&until);
        let original_type=original.current_type_schema.as_ref().unwrap().types[0].clone();
        let plan=crate::plan_xml::parse_cached_statement(detail.plan.text().unwrap(),crate::plan_xml::CachedInputContext{
            statement:&detail.context,module_parameters:detail.module_parameters.as_ref(),text:detail.statement.text(),origin:crate::plan_xml::PlanInputOrigin::OriginalNative{read:None},
        }).unwrap();
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
        let details=crate::cached_statements::details(&mut racy,std::slice::from_ref(&hash),1,&ReadSession::application_budget(), true).await.unwrap();
        assert!(racy.changed);
        let withheld=&details[&hash];
        assert!(withheld.statement.text().unwrap().contains("tvp_original_header_probe"));
        assert!(matches!(withheld.plan,crate::query_store::payload::Payload::Unavailable(PayloadUnavailable::SourceUnavailable)));
        assert!(withheld.module_parameters.is_none());
        assert!(withheld.current_type_schema.is_none());
        assert_eq!(racy.close().await,SourceShutdown::Quiescent);
        let mut fresh=ReadSession::application(pool.clone());
        let local=definition(&mut fresh,object).await;
        assert_ne!(original.context.identity.sql_handle,local.context.identity.sql_handle);
        let local_metadata=local.module_parameters.as_ref().expect("a complete header without READONLY remains a known declaration");
        assert!(local_metadata.table_parameters.is_empty(),"the same table type used as a local must never become a TVP");
        assert!(local.current_type_schema.is_none(),"current type existence cannot promote a local to a TVP or add a panel");
        assert!(matches!(local.statement.payload,DefinitionPayload::Available{..}));
        assert_eq!(fresh.close().await,SourceShutdown::Quiescent);
        eprintln!("same-body ALTER source proof: original plan={} SQL={}; current plan={} SQL={}; unchanged object_id={object}; original statement kept, unsafe plan/type withheld",
            original.context.identity.plan_handle,original.context.identity.sql_handle,local.context.identity.plan_handle,local.context.identity.sql_handle);
        batch(&mut racy.application,"DROP PROCEDURE dbo.header_probe").await;
        batch(&mut racy.application,"DROP TYPE dbo.RowsType").await;
        batch(&mut racy.application,CURRENT_TYPE_DDL).await;
        batch(&mut racy.application,&format!("CREATE PROCEDURE dbo.header_probe {TVP} {STATEMENT}")).await;
        execute(&mut racy.application,true).await;
        let recreated_identity=crate::conn::query_one(&mut racy.application,CATALOG_IDENTITY_SQL).await.unwrap();
        let recreated_object=crate::read::signed_integer(&recreated_identity,"object_id").unwrap();
        assert_ne!(object,recreated_object);
        let mut fresh=ReadSession::application(pool);
        let recreated=definition(&mut fresh,recreated_object).await;
        assert!(recreated.module_parameters.as_ref().unwrap().input(&recreated.context,"@Rows").is_some());
        let until=crate::conn::query_one(&mut racy.application,CURRENT_CLOCK_SQL).await.unwrap();
        let until=crate::read::text(&until,"observed_at").unwrap();
        assert_current_type_schema(recreated.current_type_schema.as_ref(),&recreated.context,recreated.module_parameters.as_ref().unwrap(),&recreated_identity,&until);
        assert_ne!(recreated.current_type_schema.as_ref().unwrap().types[0].backing_object,original_type.backing_object,
            "same-name type recreation has its own current native generation");
        assert_eq!(original.current_type_schema.as_ref().unwrap().types[0],original_type,
            "new current metadata cannot rewrite saved original context");
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
