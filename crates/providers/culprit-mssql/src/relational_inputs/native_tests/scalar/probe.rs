use super::*;
use crate::test_support::{actual_plan_capture_admitted, owned_case};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::CachedStatementContext;
use futures_util::future::LocalBoxFuture;

mod cache_observation;
mod memory_phases;
mod metadata;
mod post_execution;
mod stack_tests;

const MARKER: &str = "scalar_original_probe";
const SOURCE_ROWS: usize = 32_768;
const COMMON_ROWS: i64 = 24_576;
const PAYLOAD_BYTES: i64 = 256;
const STATEMENT: &str =
    "SELECT SUM(CONVERT(bigint,LEN(r.pad))) AS scalar_original_probe FROM dbo.scalar_rows r WHERE r.bucket=@bucket";
// A single same-result aggregation gives the existing scalar input a genuine execution grant. This is
// nonce-fixture workload only; it changes neither production acquisition nor application advice.
const GRANT_COUNTED_STATEMENT: &str = "SELECT COUNT_BIG(r.pad) AS scalar_original_probe FROM dbo.scalar_rows r WHERE r.bucket=@bucket GROUP BY r.pad OPTION(HASH GROUP,MAXDOP 1,MIN_GRANT_PERCENT=70,MAX_GRANT_PERCENT=70)";
const COUNTED_STATEMENT: &str =
    "SELECT COUNT_BIG(r.pad) AS scalar_original_probe FROM dbo.scalar_rows r WHERE r.bucket=@bucket";

#[derive(Clone, Copy)]
enum Projection {
    PayloadBytes,
    NonNullRows,
    GrantedGroupedRows,
}

impl Projection {
    fn statement(self) -> &'static str {
        match self {
            Self::PayloadBytes => STATEMENT,
            Self::NonNullRows => COUNTED_STATEMENT,
            Self::GrantedGroupedRows => GRANT_COUNTED_STATEMENT,
        }
    }
    fn payload_nullability(self) -> &'static str {
        match self {
            Self::PayloadBytes => "NOT NULL",
            Self::NonNullRows | Self::GrantedGroupedRows => "NULL",
        }
    }
    fn result_per_row(self) -> i64 {
        match self {
            Self::PayloadBytes => PAYLOAD_BYTES,
            Self::NonNullRows | Self::GrantedGroupedRows => 1,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum CacheExpectation<'a> {
    New,
    Reused(&'a CachedStatementContext),
}
#[derive(Clone, Copy)]
pub(super) enum IndexCoverage {
    Covering,
    LookupRequired,
}
pub(super) struct RecordedRun {
    pub plan: Plan,
    pub context: CachedStatementContext,
    pub result: i64,
}
impl RecordedRun {
    pub fn operator(&self, physical: &str) -> &culprit_core::PlanNode {
        let matching = self.plan.nodes().into_iter().filter(|node| node.op == physical).collect::<Vec<_>>();
        let [node] = matching.as_slice() else {
            panic!("one actual native {physical} required; observed {}", matching.len())
        };
        node
    }
    pub fn logical_reads(&self, physical: &str) -> f64 {
        self.operator(physical)
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.logical_reads)
            .filter(|reads| reads.is_finite() && *reads >= 0.0)
            .expect("native reads for this one data-access operator, never an inclusive sum")
    }
}
#[derive(Clone)]
pub(super) struct Actor {
    pub id: i64,
    pub generation: String,
}
pub(super) struct Probe {
    pub application: MsClient,
    pub pool: Arc<Pool>,
    pub port: u16,
    object: i64,
    actor: Actor,
    projection: Projection,
    cancellation: CollectionCancellation,
}
impl Probe {
    pub async fn new(fixture: &OwnedSourceFixture, database: &str, coverage: IndexCoverage) -> Self {
        Self::with_projection(
            fixture,
            database,
            coverage,
            Projection::PayloadBytes,
            "scalar-reuse-actor",
            CollectionCancellation::default(),
        )
        .await
    }
    pub async fn counted(fixture: &OwnedSourceFixture, database: &str, coverage: IndexCoverage) -> Self {
        Self::with_projection(
            fixture,
            database,
            coverage,
            Projection::NonNullRows,
            "scalar-reuse-actor",
            CollectionCancellation::default(),
        )
        .await
    }
    pub(super) async fn granted_counted(
        fixture: &OwnedSourceFixture,
        database: &str,
        coverage: IndexCoverage,
        application: &str,
        cancellation: &CollectionCancellation,
    ) -> Self {
        Self::with_projection(
            fixture,
            database,
            coverage,
            Projection::GrantedGroupedRows,
            application,
            cancellation.clone(),
        )
        .await
    }
    async fn with_projection(
        fixture: &OwnedSourceFixture,
        database: &str,
        coverage: IndexCoverage,
        projection: Projection,
        application_name: &str,
        cancellation: CollectionCancellation,
    ) -> Self {
        owned_case::require_active(&cancellation).unwrap();
        let mut application = crate::test_support::connect(
            &config(fixture.port, database, fixture.password(), application_name),
            SessionKind::User,
        )
        .await
        .unwrap();
        let prepared = AssertUnwindSafe(async {
            owned_case::admitted(&cancellation, verify_owned_source(&mut application)).await.unwrap();
            owned_case::batch(
                &cancellation,
                &mut application,
                "ALTER DATABASE SCOPED CONFIGURATION SET MAXDOP=1;ALTER DATABASE SCOPED CONFIGURATION SET PARAMETER_SENSITIVE_PLAN_OPTIMIZATION=OFF;ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS=ON",
            )
            .await
            .unwrap();
            owned_case::batch(
                &cancellation,
                &mut application,
                &format!(
                    "CREATE TABLE dbo.scalar_rows(id int NOT NULL CONSTRAINT pk_scalar_rows PRIMARY KEY CLUSTERED,bucket int NOT NULL,pad char(256) {})",
                    projection.payload_nullability(),
                ),
            )
            .await
            .unwrap();
            let seed = format!(
                "WITH digits(n) AS(SELECT n FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9))d(n)),numbers AS(SELECT TOP({SOURCE_ROWS}) ROW_NUMBER() OVER(ORDER BY a.n,b.n,c.n,d.n,e.n)n FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d CROSS JOIN digits e) INSERT dbo.scalar_rows SELECT CONVERT(int,n),CASE WHEN n<={COMMON_ROWS} THEN 1 ELSE CONVERT(int,n-{COMMON_ROWS}+1) END,REPLICATE('x',{PAYLOAD_BYTES}) FROM numbers"
            );
            owned_case::batch(&cancellation, &mut application, &seed).await.unwrap();
            owned_case::batch(
                &cancellation,
                &mut application,
                match coverage {
                    IndexCoverage::Covering => "CREATE INDEX ix_scalar_bucket ON dbo.scalar_rows(bucket) INCLUDE(pad)",
                    IndexCoverage::LookupRequired => "CREATE INDEX ix_scalar_bucket ON dbo.scalar_rows(bucket)",
                },
            )
            .await
            .unwrap();
            owned_case::batch(&cancellation, &mut application, "UPDATE STATISTICS dbo.scalar_rows WITH FULLSCAN")
                .await
                .unwrap();
            owned_case::batch(
                &cancellation,
                &mut application,
                &format!("CREATE PROCEDURE dbo.scalar_probe @bucket int AS {}", projection.statement()),
            )
            .await
            .unwrap();
            owned_case::require_active(&cancellation).unwrap();
            let native = crate::conn::query_one(
                &mut application,
                "SELECT /* culprit fixture */ OBJECT_ID(N'dbo.scalar_probe') AS object_id,@@SPID AS id,CONVERT(varchar(40),CAST(login_time AS datetime2),126) AS generation FROM sys.dm_exec_sessions WHERE session_id=@@SPID",
            )
            .await
            .unwrap();
            let object = crate::read::signed_integer(&native, "object_id").unwrap();
            let actor = Actor {
                id: crate::read::signed_integer(&native, "id").unwrap(),
                generation: crate::read::text(&native, "generation").unwrap(),
            };
            (object, actor)
        })
        .catch_unwind()
        .await;
        let (object, actor) = match prepared {
            Ok(prepared) => prepared,
            Err(original) => {
                let _ = owned_case::retire(application).await;
                std::panic::resume_unwind(original);
            }
        };
        let pool = Arc::new(Pool::new(config(fixture.port, database, fixture.password(), "culprit"), None));
        Self { application, pool, port: fixture.port, object, actor, projection, cancellation }
    }
    pub(super) fn cancellation(&self) -> &CollectionCancellation {
        &self.cancellation
    }
    pub fn actor(&self) -> Actor {
        self.actor.clone()
    }
    pub async fn record(&mut self, bucket: i32, expected: CacheExpectation<'_>) -> Box<RecordedRun> {
        let mut run = self.record_native(bucket, expected).await;
        run.plan = culprit_core::redact::redact_plan(&run.plan);
        run
    }

    /// This private fixture may borrow already captured original XML for an immediate pairwise comparison.
    /// Ordinary fixture capture still returns the same redacted plan; no value/hash field is introduced.
    pub async fn record_native(&mut self, bucket: i32, expected: CacheExpectation<'_>) -> Box<RecordedRun> {
        self.record_original(bucket, expected, None, None).await
    }

    pub async fn record_native_notifying(
        &mut self,
        bucket: i32,
        expected: CacheExpectation<'_>,
        submitting: &tokio::sync::Notify,
        diagnostics: Option<&std::path::Path>,
    ) -> Box<RecordedRun> {
        self.record_original(bucket, expected, Some(submitting), diagnostics).await
    }

    fn record_original<'a>(
        &'a mut self,
        bucket: i32,
        expected: CacheExpectation<'a>,
        submitting: Option<&'a tokio::sync::Notify>,
        diagnostics: Option<&'a std::path::Path>,
    ) -> LocalBoxFuture<'a, Box<RecordedRun>> {
        Box::pin(async move {
            let cancellation = self.cancellation.clone();
            let metadata_budget = super::super::expressions::reader_budget();
            let metadata_query_cap = metadata_budget.max_queries;
            let mut original_reader = ReadSession::new(self.pool.clone(), metadata_budget);
            let mut transition = None;
            let outcome = AssertUnwindSafe(async {
                assert!(matches!(bucket, 1..=3));
                let mut reader = owned_case::AdmittedRead::new(&mut original_reader, &cancellation);
                if let CacheExpectation::Reused(original) = expected {
                    phase("metadata-before", &reader);
                    match diagnostics {
                        Some(_) => {
                            let observed = *post_execution::read_before_execution(
                                &mut reader,
                                self.object,
                                original,
                                metadata_query_cap,
                            )
                            .await;
                            transition = observed.transition.map(|point| ("pre-execution-exact-absence", point));
                            observed.metadata.unwrap();
                        }
                        None => {
                            metadata::read(&mut reader, self.object, Some(original)).await.unwrap();
                        }
                    }
                    phase("metadata-before-verified", &reader);
                }
                phase("execution-owner-start", &reader);
                let sql = format!("EXEC dbo.scalar_probe @bucket={bucket}");
                let captured =
                    actual_plan_capture_admitted(&mut self.application, &sql, vec![], submitting, &cancellation)
                        .await
                        .unwrap();
                phase("execution-response-drained", &reader);
                if let Some(directory) = diagnostics {
                    post_execution::save_completed(directory, &captured.plans).unwrap();
                }
                let original = match expected {
                    CacheExpectation::New => None,
                    CacheExpectation::Reused(original) => Some(original),
                };
                phase("metadata-after", &reader);
                let current = match (diagnostics, original) {
                    (Some(_), Some(original)) => {
                        let observed = *post_execution::read(&mut reader, self.object, original).await;
                        transition = observed.transition.map(|point| ("post-execution-exact-absence", point));
                        observed.metadata.unwrap()
                    }
                    _ => metadata::read(&mut reader, self.object, original).await.unwrap(),
                };
                phase("metadata-after-verified", &reader);
                let hash = current.context.query_hash.as_ref().unwrap();
                let selected = captured
                    .plans
                    .iter()
                    .filter(|xml| super::super::expressions::has_statement(xml, hash))
                    .collect::<Vec<_>>();
                let [xml] = selected.as_slice() else { panic!("one selected native scalar statement") };
                let plan = crate::plan_xml::parse_cached_statement(
                    xml,
                    crate::plan_xml::CachedInputContext {
                        statement: &current.context,
                        module_parameters: Some(&current.module_parameters),
                        text: Some(&current.statement),
                        origin: crate::plan_xml::PlanInputOrigin::OriginalNative { read: None },
                    },
                )
                .unwrap();
                let results = captured
                    .results
                    .iter()
                    .filter(|row| row.columns().any(|(column, _)| column == MARKER))
                    .collect::<Vec<_>>();
                let [result] = results.as_slice() else { panic!("exactly one result from the same actual execution") };
                let result = crate::read::signed_integer(result, MARKER).unwrap();
                let rows = if bucket == 1 { COMMON_ROWS } else { 1 };
                assert_eq!(result, rows * self.projection.result_per_row());
                Box::new(RecordedRun { plan, context: current.context, result })
            })
            .catch_unwind()
            .await;
            // The original metadata owner closes on cancelled admission, native failure and panic alike.
            let shutdown = original_reader.close().await;
            let diagnostic = match (diagnostics, transition) {
                (Some(directory), Some((phase, transition))) => crate::test_support::save_cost_receipt(
                    directory,
                    phase,
                    &serde_json::json!({
                        "phase":phase,"transition":transition,"metadata_queries":original_reader.queries(),
                        "metadata_query_cap":metadata_query_cap,
                        "metadata_read_metrics":original_reader.read_metrics(),
                        "shutdown":format!("{shutdown:?}"),"same_original_read_session_owner":true,
                        "current_metadata_is_a_fallback":false,"original_source_unavailable_preserved":true,
                        "cache_transition_cause_established":false,"native_cost_approved":false,
                    }),
                ),
                _ => Ok(()),
            };
            let run = match outcome {
                Ok(run) => run,
                Err(original) => std::panic::resume_unwind(original),
            };
            diagnostic.unwrap();
            assert_eq!(shutdown, SourceShutdown::Quiescent);
            run
        })
    }
}
fn phase(stage: &'static str, reader: &impl crate::read::NativeRead) {
    // Fixed fixture phases and client dispatch counts only; no SQL, handles, input values or elapsed claims.
    eprintln!("owned scalar capture stage={stage}; metadata_queries={}", reader.queries());
}
