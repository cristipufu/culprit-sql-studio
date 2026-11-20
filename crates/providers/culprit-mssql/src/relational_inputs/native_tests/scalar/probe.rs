use super::*;
use crate::test_support::actual_plan_capture;
use culprit_core::telemetry::CachedStatementContext;

mod metadata;

const MARKER: &str = "scalar_original_probe";
const SOURCE_ROWS: usize = 32_768;
const COMMON_ROWS: i64 = 24_576;
const PAYLOAD_BYTES: i64 = 256;
const STATEMENT: &str =
    "SELECT SUM(CONVERT(bigint,LEN(r.pad))) AS scalar_original_probe FROM dbo.scalar_rows r WHERE r.bucket=@bucket";
const COUNTED_STATEMENT: &str =
    "SELECT COUNT_BIG(r.pad) AS scalar_original_probe FROM dbo.scalar_rows r WHERE r.bucket=@bucket";

#[derive(Clone, Copy)]
enum Projection {
    PayloadBytes,
    NonNullRows,
}

impl Projection {
    fn statement(self) -> &'static str {
        match self {
            Self::PayloadBytes => STATEMENT,
            Self::NonNullRows => COUNTED_STATEMENT,
        }
    }
    fn payload_nullability(self) -> &'static str {
        match self {
            Self::PayloadBytes => "NOT NULL",
            Self::NonNullRows => "NULL",
        }
    }
    fn result_per_row(self) -> i64 {
        match self {
            Self::PayloadBytes => PAYLOAD_BYTES,
            Self::NonNullRows => 1,
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
}
impl Probe {
    pub async fn new(fixture: &OwnedSourceFixture, database: &str, coverage: IndexCoverage) -> Self {
        Self::with_projection(fixture, database, coverage, Projection::PayloadBytes).await
    }
    pub async fn counted(fixture: &OwnedSourceFixture, database: &str, coverage: IndexCoverage) -> Self {
        Self::with_projection(fixture, database, coverage, Projection::NonNullRows).await
    }
    async fn with_projection(
        fixture: &OwnedSourceFixture,
        database: &str,
        coverage: IndexCoverage,
        projection: Projection,
    ) -> Self {
        let mut application = crate::test_support::connect(
            &config(fixture.port, database, fixture.password(), "scalar-reuse-actor"),
            SessionKind::User,
        )
        .await
        .unwrap();
        verify_owned_source(&mut application).await.unwrap();
        batch(&mut application,"ALTER DATABASE SCOPED CONFIGURATION SET MAXDOP=1;ALTER DATABASE SCOPED CONFIGURATION SET PARAMETER_SENSITIVE_PLAN_OPTIMIZATION=OFF;ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS=ON").await;
        batch(&mut application, &format!("CREATE TABLE dbo.scalar_rows(id int NOT NULL CONSTRAINT pk_scalar_rows PRIMARY KEY CLUSTERED,bucket int NOT NULL,pad char(256) {})", projection.payload_nullability())).await;
        let seed = format!(
            "WITH digits(n) AS(SELECT n FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9))d(n)),numbers AS(SELECT TOP({SOURCE_ROWS}) ROW_NUMBER() OVER(ORDER BY a.n,b.n,c.n,d.n,e.n)n FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d CROSS JOIN digits e) INSERT dbo.scalar_rows SELECT CONVERT(int,n),CASE WHEN n<={COMMON_ROWS} THEN 1 ELSE CONVERT(int,n-{COMMON_ROWS}+1) END,REPLICATE('x',{PAYLOAD_BYTES}) FROM numbers"
        );
        batch(&mut application, &seed).await;
        batch(
            &mut application,
            match coverage {
                IndexCoverage::Covering => "CREATE INDEX ix_scalar_bucket ON dbo.scalar_rows(bucket) INCLUDE(pad)",
                IndexCoverage::LookupRequired => "CREATE INDEX ix_scalar_bucket ON dbo.scalar_rows(bucket)",
            },
        )
        .await;
        batch(&mut application, "UPDATE STATISTICS dbo.scalar_rows WITH FULLSCAN").await;
        batch(
            &mut application,
            &format!("CREATE PROCEDURE dbo.scalar_probe @bucket int AS {}", projection.statement()),
        )
        .await;
        let native=crate::conn::query_one(&mut application,"SELECT /* culprit fixture */ OBJECT_ID(N'dbo.scalar_probe') AS object_id,@@SPID AS id,CONVERT(varchar(40),CAST(login_time AS datetime2),126) AS generation FROM sys.dm_exec_sessions WHERE session_id=@@SPID").await.unwrap();
        let object = crate::read::signed_integer(&native, "object_id").unwrap();
        let actor = Actor {
            id: crate::read::signed_integer(&native, "id").unwrap(),
            generation: crate::read::text(&native, "generation").unwrap(),
        };
        let pool = Arc::new(Pool::new(config(fixture.port, database, fixture.password(), "culprit"), None));
        Self { application, pool, port: fixture.port, object, actor, projection }
    }
    pub fn actor(&self) -> Actor {
        self.actor.clone()
    }
    pub async fn record(&mut self, bucket: i32, expected: CacheExpectation<'_>) -> RecordedRun {
        let mut run = self.record_native(bucket, expected).await;
        run.plan = culprit_core::redact::redact_plan(&run.plan);
        run
    }

    /// This private fixture may borrow already captured original XML for an immediate pairwise comparison.
    /// Ordinary fixture capture still returns the same redacted plan; no value/hash field is introduced.
    pub async fn record_native(&mut self, bucket: i32, expected: CacheExpectation<'_>) -> RecordedRun {
        self.record_original(bucket, expected, None).await
    }

    pub async fn record_native_notifying(
        &mut self,
        bucket: i32,
        expected: CacheExpectation<'_>,
        submitting: &tokio::sync::Notify,
    ) -> RecordedRun {
        self.record_original(bucket, expected, Some(submitting)).await
    }

    async fn record_original(
        &mut self,
        bucket: i32,
        expected: CacheExpectation<'_>,
        submitting: Option<&tokio::sync::Notify>,
    ) -> RecordedRun {
        assert!(matches!(bucket, 1..=3));
        let mut reader = ReadSession::new(self.pool.clone(), super::super::expressions::reader_budget());
        if let CacheExpectation::Reused(original) = expected {
            phase("metadata-before", &reader);
            metadata::read(&mut reader, self.object, Some(original)).await.unwrap();
            phase("metadata-before-verified", &reader);
        }
        phase("execution-owner-start", &reader);
        let sql = format!("EXEC dbo.scalar_probe @bucket={bucket}");
        let captured = match submitting {
            Some(submitting) => {
                crate::test_support::actual_plan_capture_notifying(&mut self.application, &sql, vec![], submitting)
                    .await
            }
            None => actual_plan_capture(&mut self.application, &sql, vec![]).await,
        };
        phase("execution-response-drained", &reader);
        let original = match expected {
            CacheExpectation::New => None,
            CacheExpectation::Reused(original) => Some(original),
        };
        phase("metadata-after", &reader);
        let current = metadata::read(&mut reader, self.object, original).await.unwrap();
        phase("metadata-after-verified", &reader);
        let hash = current.context.query_hash.as_ref().unwrap();
        let selected =
            captured.plans.iter().filter(|xml| super::super::expressions::has_statement(xml, hash)).collect::<Vec<_>>();
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
        let results =
            captured.results.iter().filter(|row| row.columns().any(|(column, _)| column == MARKER)).collect::<Vec<_>>();
        let [result] = results.as_slice() else { panic!("exactly one result from the same actual execution") };
        let result = crate::read::signed_integer(result, MARKER).unwrap();
        let rows = if bucket == 1 { COMMON_ROWS } else { 1 };
        assert_eq!(result, rows * self.projection.result_per_row());
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        RecordedRun { plan, context: current.context, result }
    }
}
fn phase(stage: &'static str, reader: &ReadSession) {
    // Fixed fixture phases and client dispatch counts only; no SQL, handles, input values or elapsed claims.
    eprintln!("owned scalar capture stage={stage}; metadata_queries={}", reader.queries());
}
