//! Application execution and actual XML/results retain one existing owner. Metadata rechecks are bounded and
//! close their native reader even when a fixture assertion panics; no new production instrumentation exists.
use super::{
    identity::{self, RoutingExpectation},
    *,
};
use crate::{
    conn::{MsClient, Pool, SessionKind},
    read::ReadSession,
    test_support::{actual_plan_capture, batch, config, parameter_variants as skew, verify_owned_source},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::CachedStatementContext;
use std::sync::Arc;

pub(super) const CALLS_PER_VARIANT: usize = 50;
const MAX_EXECUTIONS: usize = CALLS_PER_VARIANT * 2 + 2;
pub(super) struct Run {
    pub recorded: Box<super::super::probe::RecordedRun>,
    pub variant: Option<u32>,
    pub plan_hash: String,
}
pub(super) struct Probe<'a> {
    pub application: MsClient,
    pub pool: Arc<Pool>,
    pub object: i64,
    database_id: u32,
    pub actor: super::super::probe::Actor,
    cancellation: &'a CollectionCancellation,
    executions: usize,
}
impl<'a> Probe<'a> {
    pub async fn new(
        fixture: &crate::test_support::OwnedSourceFixture,
        database: &str,
        cancellation: &'a CollectionCancellation,
    ) -> Self {
        active(cancellation);
        let mut application = crate::test_support::connect(
            &config(fixture.port, database, fixture.password(), "psp-healthy-actor"),
            SessionKind::User,
        )
        .await
        .unwrap();
        verify_owned_source(&mut application).await.unwrap();
        batch(&mut application,"ALTER DATABASE SCOPED CONFIGURATION SET MAXDOP=1;ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS=ON;ALTER DATABASE SCOPED CONFIGURATION SET PARAMETER_SNIFFING=ON;ALTER DATABASE SCOPED CONFIGURATION SET PARAMETER_SENSITIVE_PLAN_OPTIMIZATION=ON;ALTER DATABASE CURRENT SET QUERY_STORE=ON(OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL,WAIT_STATS_CAPTURE_MODE=OFF)").await;
        active(cancellation);
        crate::test_support::query_store_ready(&mut application).await;
        active(cancellation);
        let seed = skew::seed(&mut application, cancellation).await;
        eprintln!("owned PSP original seed object={} setup client_ms={}", seed.object_id, seed.client_elapsed_ms);
        active(cancellation);
        batch(
            &mut application,
            &format!("CREATE PROCEDURE dbo.parameter_variant_probe @bucket int AS {}", skew::STATEMENT),
        )
        .await;
        let native=crate::conn::query_one(&mut application,"SELECT /* culprit fixture */ OBJECT_ID(N'dbo.parameter_variant_probe') AS object_id,DB_ID() AS database_id,@@SPID AS id,CONVERT(varchar(40),login_time,126) AS generation FROM sys.dm_exec_sessions WHERE session_id=@@SPID").await.unwrap();
        let object = crate::read::signed_integer(&native, "object_id").unwrap();
        let database_id = u32::try_from(crate::read::integer(&native, "database_id").unwrap()).unwrap();
        assert!(database_id > 0);
        let actor = super::super::probe::Actor {
            id: crate::read::signed_integer(&native, "id").unwrap(),
            generation: crate::read::text(&native, "generation").unwrap(),
        };
        let pool = Arc::new(Pool::new(config(fixture.port, database, fixture.password(), "culprit"), None));
        Self { application, pool, object, database_id, actor, cancellation, executions: 0 }
    }
    pub async fn record(
        &mut self,
        bucket: i32,
        routing: RoutingExpectation,
        original: Option<&CachedStatementContext>,
    ) -> Run {
        // Keep application capture, exact binding and metadata close inside the same owned phase.
        Box::pin(async {
            active(self.cancellation);
            self.executions += 1;
            assert!(self.executions <= MAX_EXECUTIONS, "no workload retries or adaptive growth");
            let mut reader = ReadSession::new(self.pool.clone(), super::super::super::expressions::reader_budget());
            let work = AssertUnwindSafe(async {
                if let Some(original) = original {
                    let before = super::super::super::exact_definitions(&mut reader, original).await;
                    assert!(
                        before.iter().any(|row| row.fact.context.database_id == original.database_id
                            && row.fact.context.identity.key().unwrap() == original.identity.key().unwrap()),
                        "the exact original generation must exist before execution"
                    );
                }
                active(self.cancellation);
                let captured = actual_plan_capture(
                    &mut self.application,
                    &format!("EXEC dbo.parameter_variant_probe @bucket={bucket}"),
                    vec![],
                )
                .await;
                active(self.cancellation);
                let current = match routing {
                    RoutingExpectation::Variant => super::cache::definitions(
                        &mut reader,
                        self.database_id,
                        &captured.plans,
                        original,
                        self.cancellation,
                    )
                    .await
                    .unwrap(),
                    RoutingExpectation::Ordinary => {
                        super::super::super::named_definitions(&mut reader, self.object, skew::MARKER).await
                    }
                };
                let selected = identity::bind(&captured.plans, &current, original, routing);
                let definition = &current[selected.definition];
                let plan = crate::plan_xml::parse_cached_statement(
                    &captured.plans[selected.xml],
                    crate::plan_xml::CachedInputContext {
                        statement: &definition.fact.context,
                        module_parameters: definition.fact.module_parameters.as_ref(),
                        text: Some(&definition.original_statement),
                        origin: crate::plan_xml::PlanInputOrigin::OriginalNative { read: None },
                    },
                )
                .unwrap();
                let result = captured
                    .results
                    .iter()
                    .filter(|row| row.columns().any(|(name, _)| name == skew::MARKER))
                    .collect::<Vec<_>>();
                let [result] = result.as_slice() else {
                    panic!("exactly one result from the same measured application execution")
                };
                let result = crate::read::signed_integer(result, skew::MARKER).unwrap();
                assert_eq!(
                    result,
                    skew::expected_result(bucket),
                    "neither routing nor intervention may change query semantics"
                );
                let plan = culprit_core::redact::redact_plan(&plan);
                Run {
                    recorded: Box::new(super::super::probe::RecordedRun {
                        plan,
                        context: definition.fact.context.clone(),
                        result,
                    }),
                    variant: selected.variant,
                    plan_hash: definition.fact.query_plan_hash.clone().unwrap(),
                }
            })
            .catch_unwind()
            .await;
            let queries = reader.queries();
            let shutdown = reader.close().await;
            assert_eq!(shutdown, SourceShutdown::Quiescent, "the original metadata owner must settle");
            assert!(queries <= super::super::super::expressions::reader_budget().max_queries);
            match work {
                Ok(run) => run,
                Err(error) => std::panic::resume_unwind(error),
            }
        })
        .await
    }
    pub async fn disable_psp(&mut self) {
        active(self.cancellation);
        // A change only in the owned database. No statistics/index/data change, forcing or production advice.
        batch(&mut self.application,"ALTER DATABASE SCOPED CONFIGURATION SET PARAMETER_SENSITIVE_PLAN_OPTIMIZATION=OFF;EXEC sys.sp_recompile N'dbo.parameter_variant_probe'").await;
    }
    pub async fn flush(&mut self) {
        active(self.cancellation);
        batch(&mut self.application, "EXEC sys.sp_query_store_flush_db").await;
    }
}
fn active(cancellation: &CollectionCancellation) {
    assert!(!cancellation.is_cancelled(), "owned case deadline: no subsequent application or metadata dispatch");
}
