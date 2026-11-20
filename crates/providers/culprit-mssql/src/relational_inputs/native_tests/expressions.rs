//! Only this explicitly admitted disposable fixture executes TVP workload SQL. It does not qualify production
//! reader costs or establish harmful parameter reuse from the deliberately controlled input change.
use super::*;
use crate::test_support::{OwnedSourceFixture, actual_plans, verify_owned_source};
use culprit_core::{NodeKind, PlanKind};

const LARGE_INPUT: usize = 20_000;
const FILTERED_ROWS: f64 = 10_000.0;
const READER_QUERIES: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Counted,
    Absolute,
    Healthy,
    DerivedCounted,
    DerivedHealthy,
    CteCounted,
    CteHealthy,
    ProjectedCounted,
    ProjectedHealthy,
    AliasedCounted,
    AliasedHealthy,
    ChainCounted,
    ChainHealthy,
}
impl Probe {
    fn name(self) -> &'static str {
        match self {
            Self::Counted => "count_probe",
            Self::Absolute => "abs_probe",
            Self::Healthy => "healthy_probe",
            Self::DerivedCounted => "derived_count_probe",
            Self::DerivedHealthy => "derived_healthy_probe",
            Self::CteCounted => "cte_count_probe",
            Self::CteHealthy => "cte_healthy_probe",
            Self::ProjectedCounted => "projected_count_probe",
            Self::ProjectedHealthy => "projected_healthy_probe",
            Self::AliasedCounted => "aliased_count_probe",
            Self::AliasedHealthy => "aliased_healthy_probe",
            Self::ChainCounted => "chain_count_probe",
            Self::ChainHealthy => "chain_healthy_probe",
        }
    }
    fn statement(self) -> &'static str {
        match self {
            Self::Counted => "SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM @Rows r WHERE (r.id > 10000)",
            Self::Absolute => "SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM @Rows r WHERE (ABS(r.id) > 10000)",
            Self::Healthy => "SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM @Rows r",
            Self::DerivedCounted => {
                "SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM (SELECT * FROM @Rows AS r) AS d WHERE (d.id > 10000)"
            }
            Self::DerivedHealthy => {
                "SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM (SELECT * FROM @Rows AS r) AS d"
            }
            Self::CteCounted => {
                "WITH input AS (SELECT * FROM @Rows AS r) SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM input AS d WHERE (d.id > 10000)"
            }
            Self::CteHealthy => {
                "WITH input AS (SELECT * FROM @Rows AS r) SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM input AS d"
            }
            Self::ProjectedCounted => {
                "WITH input AS (SELECT r.id FROM @Rows AS r) SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM input AS d WHERE (d.id > 10000)"
            }
            Self::ProjectedHealthy => {
                "WITH input AS (SELECT r.id FROM @Rows AS r) SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM input AS d"
            }
            Self::AliasedCounted => {
                "WITH input AS (SELECT r.id AS input_id FROM @Rows AS r) SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM input AS d WHERE (d.input_id > 10000)"
            }
            Self::AliasedHealthy => {
                "WITH input AS (SELECT r.id AS input_id FROM @Rows AS r) SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM input AS d"
            }
            Self::ChainCounted => {
                "WITH seed AS (SELECT r.id FROM @Rows AS r), input AS (SELECT p.id FROM seed AS p) SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM input AS d WHERE (d.id > 10000)"
            }
            Self::ChainHealthy => {
                "WITH seed AS (SELECT r.id FROM @Rows AS r), input AS (SELECT p.id FROM seed AS p) SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM input AS d"
            }
        }
    }
    fn is_healthy(self) -> bool {
        matches!(
            self,
            Self::Healthy
                | Self::DerivedHealthy
                | Self::CteHealthy
                | Self::ProjectedHealthy
                | Self::AliasedHealthy
                | Self::ChainHealthy
        )
    }
    fn execution(self, rows: usize) -> String {
        assert!(matches!(rows, 1 | LARGE_INPUT));
        // Five fixed decimal sets bound setup at 100k combinations; TOP admits only the fixed 20k input ceiling.
        format!(
            "DECLARE @input dbo.RowsType;WITH digits(n) AS (SELECT n FROM (VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)) d(n)) INSERT @input(id) SELECT TOP({rows}) CONVERT(int,ROW_NUMBER() OVER(ORDER BY a.n,b.n,c.n,d.n,e.n)) FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d CROSS JOIN digits e;EXEC dbo.{} @Rows=@input",
            self.name()
        )
    }
}

pub(super) fn reader_budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: READER_QUERIES,
        max_snapshot_rows: 1024,
        page_rows: 64,
        page_bytes: 1024 * 1024,
        total_bytes: 4 * 1024 * 1024,
        max_sql_bytes: 4096,
        max_plan_bytes: 64 * 1024,
        statement_timeout_ms: 3000,
        total_timeout_ms: 15000,
        ..Default::default()
    }
}

pub(super) fn has_statement(xml: &str, hash: &str) -> bool {
    culprit_core::xml::read(xml, |document| {
        document.is_ok_and(|document| {
            document.descendants().any(|node| {
                node.is_element()
                    && node.tag_name().name() == "StmtSimple"
                    && node.attribute("QueryHash").is_some_and(|native| native.eq_ignore_ascii_case(hash))
            })
        })
    })
}

fn assert_native_evidence(case: Probe, plan: &Plan) {
    assert_eq!(plan.kind, PlanKind::Actual);
    assert_eq!(plan.root.act_rows, Some(1.0), "COUNT_BIG returns one result row, not the TVP population");
    let inputs = plan.nodes().into_iter().filter(|node| node.facts().relational_input.is_some()).collect::<Vec<_>>();
    let [input] = inputs.as_slice() else { panic!("one native variable input in {case:?}: {inputs:?}") };
    eprintln!(
        "owned TVP {case:?}: native node={},alias={:?},kind={:?}",
        input.id,
        input.facts().table_alias,
        input.facts().relational_input
    );
    if matches!(
        case,
        Probe::DerivedCounted
            | Probe::DerivedHealthy
            | Probe::CteCounted
            | Probe::CteHealthy
            | Probe::ProjectedCounted
            | Probe::ProjectedHealthy
            | Probe::AliasedCounted
            | Probe::AliasedHealthy
            | Probe::ChainCounted
            | Probe::ChainHealthy
    ) {
        assert_eq!(
            input.facts().table_alias.as_deref(),
            Some("r"),
            "native passthrough input must preserve the original inner alias; the outer alias cannot substitute"
        );
    }
    assert_eq!(
        input.facts().relational_input.unwrap().qualified_kind(),
        RelationalInputKind::TableValuedParameter,
        "only the original module and exact native access can establish TVP provenance"
    );
    assert_eq!(input.kind, NodeKind::Scan);
    let access = input.facts().access_rows.expect("native rows-read must be present, never substituted from output");
    assert_eq!(access.actual_total, Some(LARGE_INPUT as f64));
    assert!(input.facts().scan_completion.as_ref().unwrap().is_single_completed_serial_scan());
    assert_eq!(input.facts().serial_execution_count.as_deref(), Some("1"));
    let insights = culprit_core::insights::insights(plan);
    if case.is_healthy() {
        assert_eq!(
            access.estimated_per_execution,
            Some(LARGE_INPUT as f64),
            "healthy control must actually compile for the large TVP"
        );
        assert_eq!(input.act_rows, Some(LARGE_INPUT as f64));
        assert!(
            insights.iter().all(|insight| !matches!(insight.rule.as_str(), "misestimate" | "relational-input-access")),
            "a healthy large TVP is not a fault: {insights:?}"
        );
    } else {
        assert!(
            access
                .estimated_per_execution
                .is_some_and(|estimate| estimate > 0.0 && estimate * 10.0 < LARGE_INPUT as f64),
            "the owned small-input prime must actually retain a material native estimate difference"
        );
        assert!(
            plan.nodes().iter().any(|node| node.act_rows == Some(FILTERED_ROWS)),
            "native filtering must retain 10k rows before aggregation"
        );
        let concern = insights
            .iter()
            .find(|insight| {
                matches!(insight.rule.as_str(), "misestimate" | "relational-input-access")
                    && insight.nodes.contains(&input.id)
            })
            .expect("measured native input estimate discrepancy");
        assert!(concern.fix_sql.is_none());
        assert!(concern.fix.contains("table-valued parameter"));
        if matches!(
            case,
            Probe::Counted
                | Probe::DerivedCounted
                | Probe::CteCounted
                | Probe::ProjectedCounted
                | Probe::AliasedCounted
                | Probe::ChainCounted
        ) {
            assert_eq!(input.act_rows, Some(FILTERED_ROWS));
            let counts = culprit_core::t!(
                "insight.relational_input.completed_input_counts",
                estimated = culprit_core::format::count(access.estimated_per_execution.unwrap()),
                actual = "20K"
            );
            assert!(concern.detail.contains(&counts), "the serial native path must support the full-input distinction");
            assert!(concern.detail.contains(&culprit_core::t!("insight.relational_input.root_output", actual = "1")));
        }
    }
    assert!(insights.iter().all(|insight| insight.fix_sql.is_none()));
    eprintln!(
        "owned TVP {case:?}: native estimated input={:?},accessed={:?},scan output={:?},aggregate output={:?}",
        access.estimated_per_execution, access.actual_total, input.act_rows, plan.root.act_rows
    );
}

async fn capture_probe(application: &mut MsClient, pool: &Arc<Pool>, case: Probe) {
    batch(
        application,
        &format!("CREATE PROCEDURE dbo.{} @Rows dbo.RowsType READONLY AS {}", case.name(), case.statement()),
    )
    .await;
    batch(application, &case.execution(if case.is_healthy() { LARGE_INPUT } else { 1 })).await;
    let object = crate::conn::query_one(
        application,
        &format!("SELECT /* culprit fixture */ OBJECT_ID(N'dbo.{}') AS object_id", case.name()),
    )
    .await
    .unwrap();
    let object = crate::read::signed_integer(&object, "object_id").unwrap();
    let budget = reader_budget();
    let mut reader = ReadSession::new(pool.clone(), budget.clone());
    let original = definition(&mut reader, object).await;
    let hash = original.context.query_hash.clone().unwrap();
    let details =
        crate::cached_statements::details(&mut reader, std::slice::from_ref(&hash), 1, &budget, true).await.unwrap();
    let detail = &details[&hash];
    assert_eq!(detail.context.identity, original.context.identity);
    assert!(detail.module_parameters.as_ref().unwrap().input(&detail.context, "@Rows").is_some());
    for schema in [&original.current_type_schema, &detail.current_type_schema] {
        let schema = schema.as_ref().expect("the owned declared TVP must retain its current heap type metadata");
        assert_eq!(schema.status, culprit_core::telemetry::MssqlCurrentTypeSchemaStatus::Available);
        let [data_type] = schema.types.as_slice() else { panic!("one owned current TVP type") };
        assert!(data_type.indexes.is_empty(), "a native heap has an explicit empty index array");
        let [column] = data_type.columns.as_slice() else { panic!("one native input column") };
        assert_eq!((column.name.as_str(), column.type_name.as_str(), column.max_length), ("id", "int", 4));
    }

    let plans = actual_plans(application, &case.execution(LARGE_INPUT), vec![]).await;
    let current = definition(&mut reader, object).await;
    assert_eq!(
        current.context.identity, original.context.identity,
        "a changed native cache generation cannot supply comparison authority"
    );
    let selected = plans.iter().filter(|xml| has_statement(xml, &hash)).collect::<Vec<_>>();
    let [xml] = selected.as_slice() else { panic!("one original statement actual plan for {case:?}") };
    let plan = crate::plan_xml::parse_cached_statement(
        xml,
        crate::plan_xml::CachedInputContext {
            statement: &detail.context,
            module_parameters: detail.module_parameters.as_ref(),
            text: detail.statement.text(),
            origin: crate::plan_xml::PlanInputOrigin::OriginalNative { read: None },
        },
    )
    .unwrap();
    assert!(reader.queries() <= READER_QUERIES);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_native_evidence(case, &plan);
}

async fn capture_cases(fixture: &OwnedSourceFixture, database: &str, cases: &[Probe]) {
    let mut application = crate::test_support::connect(
        &config(fixture.port, database, fixture.password(), "tvp-expression-actor"),
        SessionKind::User,
    )
    .await
    .unwrap();
    verify_owned_source(&mut application).await.unwrap();
    // Fixture-only serial topology: no query hint is inserted into product advice or the binding grammar.
    batch(
        &mut application,
        "ALTER DATABASE SCOPED CONFIGURATION SET MAXDOP=1;CREATE TYPE dbo.RowsType AS TABLE(id int NOT NULL)",
    )
    .await;
    let pool = Arc::new(Pool::new(config(fixture.port, database, fixture.password(), "culprit"), None));
    for &case in cases {
        capture_probe(&mut application, &pool, case).await;
    }
    application.close().await.unwrap();
}

async fn capture_probes(fixture: &OwnedSourceFixture, database: &str) {
    capture_cases(fixture, database, &[Probe::Counted, Probe::Absolute, Probe::Healthy]).await;
}

async fn capture_derived_probes(fixture: &OwnedSourceFixture, database: &str) {
    capture_cases(fixture, database, &[Probe::DerivedCounted, Probe::DerivedHealthy]).await;
}

async fn capture_cte_probes(fixture: &OwnedSourceFixture, database: &str) {
    capture_cases(fixture, database, &[Probe::CteCounted, Probe::CteHealthy]).await;
}

async fn capture_projected_probes(fixture: &OwnedSourceFixture, database: &str) {
    capture_cases(fixture, database, &[Probe::ProjectedCounted, Probe::ProjectedHealthy]).await;
}

#[tokio::test]
#[ignore = "requires SQL Server 2022+ owned loopback fixture, full nonce admission and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_count_and_abs_actual_plans_bind_tvp_input_and_preserve_a_healthy_large_control() {
    super::fixture::run("count", capture_probes).await;
}

#[tokio::test]
#[ignore = "requires SQL Server 2022+ owned loopback fixture, full nonce admission and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_derived_count_actual_plans_preserve_the_original_inner_alias_and_a_healthy_large_control() {
    super::fixture::run("derived", capture_derived_probes).await;
}

#[tokio::test]
#[ignore = "requires SQL Server 2022+ owned loopback fixture, full nonce admission and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_cte_count_actual_plans_preserve_the_original_inner_alias_and_a_healthy_large_control() {
    super::fixture::run("cte", capture_cte_probes).await;
}

#[tokio::test]
#[ignore = "requires SQL Server 2022+ owned loopback fixture, full nonce admission and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_projected_cte_count_actual_plans_preserve_original_alias_and_healthy_input() {
    super::fixture::run("projected", capture_projected_probes).await;
}

async fn capture_aliased_probes(fixture: &OwnedSourceFixture, database: &str) {
    capture_cases(fixture, database, &[Probe::AliasedCounted, Probe::AliasedHealthy]).await;
}

#[tokio::test]
#[ignore = "requires SQL Server 2022+ owned loopback fixture, full nonce admission and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_explicit_column_aliases_keep_original_native_input_and_a_healthy_large_control() {
    super::fixture::run("aliases", capture_aliased_probes).await;
}

async fn capture_chained_cte_probes(fixture: &OwnedSourceFixture, database: &str) {
    capture_cases(fixture, database, &[Probe::ChainCounted, Probe::ChainHealthy]).await;
}

#[tokio::test]
#[ignore = "requires SQL Server 2022+ owned loopback fixture, full nonce admission and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_two_cte_chain_keeps_original_native_input_and_a_healthy_large_control() {
    super::fixture::run("chain", capture_chained_cte_probes).await;
}
