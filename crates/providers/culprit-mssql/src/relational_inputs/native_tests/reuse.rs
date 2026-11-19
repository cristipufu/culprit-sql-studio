//! Deliberate workload changes and recompilation are confined to this admitted disposable fixture. Production
//! readers observe retained evidence and never execute these application calls or change a plan's policy.
use super::*;
use crate::test_support::{OwnedSourceFixture, actual_plans, verify_owned_source};
use culprit_core::{NodeKind, telemetry::CachedStatementIdentity};

const INPUT_ROWS: usize = 20_000;
const SCOPED_READER_QUERY_LIMIT: u32 = 24;
const INNER_ROWS_PER_SEEK: usize = 200;
const GRANT_STATEMENT: &str = "SELECT DISTINCT r.pad AS tvp_original_header_probe FROM @Rows r";
const SELECTIVITY_STATEMENT: &str = "SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM @Rows r WHERE r.id=1";

struct RecordedRun {
    plan: Plan,
    identity: CachedStatementIdentity,
}

#[derive(Clone, Copy)]
enum InputShape {
    WideDistinct,
    DistinctIds,
    RepeatedId,
}

impl InputShape {
    fn table_type(self) -> &'static str {
        match self {
            Self::WideDistinct => "WideRows",
            Self::DistinctIds | Self::RepeatedId => "RowsType",
        }
    }

    fn columns(self) -> &'static str {
        match self {
            Self::WideDistinct => "n,RIGHT(REPLICATE('0',10)+CONVERT(varchar(10),n),10)+REPLICATE('x',2000)",
            Self::DistinctIds => "n",
            Self::RepeatedId => "1",
        }
    }
}

async fn record_run(
    application: &mut MsClient,
    pool: &Arc<Pool>,
    procedure: &str,
    sql: &str,
    source_fixture: Option<&OwnedSourceFixture>,
) -> RecordedRun {
    let object = crate::conn::query_one(
        application,
        &format!("SELECT /* culprit fixture */ OBJECT_ID(N'dbo.{procedure}') AS object_id"),
    )
    .await
    .unwrap();
    let object = crate::read::signed_integer(&object, "object_id").unwrap();
    let mut budget = super::expressions::reader_budget();
    if source_fixture.is_some() {
        // The existing source authority reader and its independent recheck are part of this owned test budget.
        budget.max_queries = SCOPED_READER_QUERY_LIMIT;
    }
    let mut reader = ReadSession::new(pool.clone(), budget.clone());
    let source = match source_fixture {
        Some(fixture) => {
            Some(crate::local_source::Context::read(&mut reader, "127.0.0.1", fixture.port).await.unwrap())
        }
        None => None,
    };
    let originals = named_definitions(&mut reader, object, "tvp_original_header_probe").await;
    let [original] = originals.as_slice() else { panic!("one original statement in {procedure}") };
    let hash = original.fact.context.query_hash.as_ref().unwrap();
    let details =
        crate::cached_statements::details(&mut reader, std::slice::from_ref(hash), 1, &budget, true).await.unwrap();
    let detail = &details[hash];
    assert_eq!(detail.context.identity, original.fact.context.identity);
    let plans = actual_plans(application, sql, vec![]).await;
    let current = definition(&mut reader, object).await;
    assert_eq!(
        current.context.identity, original.fact.context.identity,
        "the recorded call must reuse this native entry"
    );
    let source = match (source, source_fixture) {
        (Some(before), Some(fixture)) => {
            let after = before.recheck(&mut reader, "127.0.0.1", fixture.port).await.unwrap();
            Some(before.completed(&after).unwrap())
        }
        (None, None) => None,
        _ => unreachable!(),
    };
    let selected = plans.iter().filter(|xml| super::expressions::has_statement(xml, hash)).collect::<Vec<_>>();
    let [xml] = selected.as_slice() else { panic!("one original native statement in {procedure}") };
    let plan = crate::plan_xml::parse_cached_statement(
        xml,
        crate::plan_xml::CachedInputContext {
            statement: &detail.context,
            module_parameters: detail.module_parameters.as_ref(),
            text: Some(&original.original_statement),
            origin: crate::plan_xml::PlanInputOrigin::OriginalNative { read: source.as_ref() },
        },
    )
    .unwrap();
    assert!(reader.queries() <= budget.max_queries);
    eprintln!("owned TVP original/native namespace reader: queries={}", reader.queries());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    RecordedRun { plan, identity: original.fact.context.identity.clone() }
}

fn input(plan: &Plan) -> &culprit_core::PlanNode {
    let inputs = plan
        .nodes()
        .into_iter()
        .filter(|node| {
            node.facts()
                .relational_input
                .is_some_and(|input| input.qualified_kind() == RelationalInputKind::TableValuedParameter)
        })
        .collect::<Vec<_>>();
    let [input] = inputs.as_slice() else { panic!("one positively qualified TVP input") };
    assert_eq!(input.kind, NodeKind::Scan);
    assert!(input.facts().scan_completion.as_ref().unwrap().is_single_completed_serial_scan());
    input
}

fn populate(procedure: &str, rows: usize, shape: InputShape) -> String {
    assert!(matches!(rows, 1 | INPUT_ROWS));
    let relation = "WITH digits(n) AS (SELECT n FROM (VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)) d(n)), numbers AS (SELECT TOP(20000) ROW_NUMBER() OVER(ORDER BY a.n,b.n,c.n,d.n,e.n) n FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d CROSS JOIN digits e)";
    let columns = shape.columns();
    let table_type = shape.table_type();
    format!(
        "DECLARE @input dbo.{table_type};{relation} INSERT @input SELECT TOP({rows}) {columns} FROM numbers;EXEC dbo.{procedure} @Rows=@input"
    )
}

async fn actors(fixture: &OwnedSourceFixture, database: &str) -> (MsClient, Arc<Pool>) {
    let mut application = crate::test_support::connect(
        &config(fixture.port, database, fixture.password(), "tvp-causal-actor"),
        SessionKind::User,
    )
    .await
    .unwrap();
    verify_owned_source(&mut application).await.unwrap();
    batch(&mut application, "ALTER DATABASE SCOPED CONFIGURATION SET MAXDOP=1").await;
    let pool = Arc::new(Pool::new(config(fixture.port, database, fixture.password(), "culprit"), None));
    (application, pool)
}

async fn compare_grant_after_fresh_compilation(fixture: &OwnedSourceFixture, database: &str) {
    let (mut application, pool) = actors(fixture, database).await;
    batch(&mut application, "CREATE TYPE dbo.WideRows AS TABLE(id int NOT NULL,pad varchar(2048) NOT NULL)").await;
    batch(
        &mut application,
        &format!("CREATE PROCEDURE dbo.grant_probe @Rows dbo.WideRows READONLY AS {GRANT_STATEMENT}"),
    )
    .await;
    batch(&mut application, &populate("grant_probe", INPUT_ROWS, InputShape::WideDistinct)).await;
    let small = populate("grant_probe", 1, InputShape::WideDistinct);
    let reused = record_run(&mut application, &pool, "grant_probe", &small, None).await;
    let access = input(&reused.plan).facts().access_rows.unwrap();
    assert_eq!(access.estimated_per_execution, Some(INPUT_ROWS as f64));
    assert_eq!(access.actual_total, Some(1.0));
    let insight = culprit_core::insights::insights(&reused.plan)
        .into_iter()
        .find(|insight| insight.rule == "memory-grant")
        .expect("the controlled large-to-small reuse must actually retain an oversized native grant");
    assert!(insight.nodes.contains(&input(&reused.plan).id));
    assert!(insight.fix_sql.is_none());
    assert_eq!(insight.fix, culprit_core::t!("insight.memory_grant.tvp.investigate"));

    // An owned-only intervention checks the prediction with unchanged SQL/input/serial execution. It establishes
    // the observed grant difference, not production throughput harm or universal safety of recompilation.
    batch(&mut application, "EXEC sys.sp_recompile N'dbo.grant_probe'").await;
    batch(&mut application, &small).await;
    let fresh = record_run(&mut application, &pool, "grant_probe", &small, None).await;
    assert_ne!(fresh.identity, reused.identity, "fresh native compilation needs a new incarnation");
    assert_eq!(input(&fresh.plan).facts().access_rows.unwrap().estimated_per_execution, Some(1.0));
    assert_eq!(fresh.plan.root.act_rows, reused.plan.root.act_rows);
    let before = reused.plan.facts().memory_granted_kb.unwrap();
    let after = fresh.plan.facts().memory_granted_kb.unwrap();
    assert!(after < before, "the predicted execution-memory reservation must actually decrease: {before} -> {after}");
    assert!(!culprit_core::insights::insights(&fresh.plan).iter().any(|insight| insight.rule == "memory-grant"));
    eprintln!(
        "owned TVP large-to-small/fresh compile: same1-row input; granted KiB {before}->{after}; native identities changed"
    );
    application.close().await.unwrap();
}

async fn compare_equal_input_different_distribution(fixture: &OwnedSourceFixture, database: &str) {
    let (mut application, pool) = actors(fixture, database).await;
    batch(&mut application, "CREATE TYPE dbo.RowsType AS TABLE(id int NOT NULL)").await;
    batch(
        &mut application,
        &format!("CREATE PROCEDURE dbo.skew_probe @Rows dbo.RowsType READONLY AS {SELECTIVITY_STATEMENT}"),
    )
    .await;
    let uniform = populate("skew_probe", INPUT_ROWS, InputShape::DistinctIds);
    let skewed = populate("skew_probe", INPUT_ROWS, InputShape::RepeatedId);
    batch(&mut application, &uniform).await;
    let before = record_run(&mut application, &pool, "skew_probe", &uniform, None).await;
    let after = record_run(&mut application, &pool, "skew_probe", &skewed, None).await;
    assert_eq!(before.identity, after.identity, "the input distribution comparison must retain the native plan entry");
    for run in [&before, &after] {
        let access = input(&run.plan).facts().access_rows.unwrap();
        assert_eq!(access.actual_total, Some(INPUT_ROWS as f64));
        assert_eq!(access.estimated_per_execution, Some(INPUT_ROWS as f64));
        assert_eq!(run.plan.root.act_rows, Some(1.0), "aggregate output is not the input or kept population");
    }
    assert_eq!(input(&before.plan).act_rows, Some(1.0));
    assert_eq!(input(&after.plan).act_rows, Some(INPUT_ROWS as f64));
    let insights = culprit_core::insights::insights(&after.plan);
    let concern =
        insights.iter().find(|insight| insight.rule == "misestimate").expect("native selectivity discrepancy");
    assert!(concern.detail.contains(&culprit_core::t!("insight.relational_input.selectivity")));
    assert!(!concern.detail.contains(&culprit_core::t!("insight.relational_input.completed_input_volume")));
    assert!(concern.fix_sql.is_none());
    eprintln!("owned TVP distribution: same20k input and native cache entry, filter output1->20k, aggregate output1");
    application.close().await.unwrap();
}

/// Bounded typed evidence for the owned fixture; no raw SQL, client labels or application values are logged.
fn trace_loop_populations(plan: &Plan) {
    const MAX_DIAGNOSTIC_NODES: usize = 32;
    let nodes = plan.nodes();
    eprintln!(
        "owned TVP loop population: engine={:?} kind={:?} root={} nodes={} shown={}",
        plan.engine,
        plan.kind,
        plan.root.id,
        nodes.len(),
        nodes.len().min(MAX_DIAGNOSTIC_NODES)
    );
    for node in nodes.into_iter().take(MAX_DIAGNOSTIC_NODES) {
        let facts = node.facts();
        eprintln!(
            "node={} kind={:?} children={:?} estimated={} actual={:?} executions={:?} serial={:?} parallel={:?} estimate_unknown={} row_goal={:?} inner_join={:?} access={:?} input={:?} completed_scan={} object={} index={} seek={} predicate={}",
            node.id,
            node.kind,
            node.children.iter().take(MAX_DIAGNOSTIC_NODES).map(|child| child.id).collect::<Vec<_>>(),
            node.est_rows,
            node.act_rows,
            node.act_executions,
            facts.serial_execution_count,
            facts.parallel_threads,
            facts.estimated_rows_unavailable,
            facts.estimated_rows_without_goal,
            facts.inner_join,
            facts.access_rows,
            facts.relational_input,
            facts.scan_completion.as_ref().is_some_and(|completion| completion.is_single_completed_serial_scan()),
            node.object.is_some(),
            node.index.is_some(),
            node.seek_predicate.is_some(),
            node.predicate.is_some(),
        );
    }
}

async fn compare_wrapped_loop_after_fresh_compilation(fixture: &OwnedSourceFixture, database: &str) {
    let (mut application, pool) = actors(fixture, database).await;
    batch(&mut application, "CREATE TYPE dbo.RowsType AS TABLE(id int NOT NULL);CREATE TABLE dbo.Items(group_id int NOT NULL);CREATE CLUSTERED INDEX IX_Items_group ON dbo.Items(group_id)").await;
    // Two hundred equal indexed keys make each seek genuinely material without returning a large client result.
    batch(&mut application, "WITH digits(n) AS (SELECT n FROM (VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)) d(n)) INSERT dbo.Items SELECT 2 FROM digits a CROSS JOIN digits b CROSS JOIN (VALUES(0),(1)) c(n)").await;
    batch(&mut application, "CREATE PROCEDURE dbo.wrapped_probe @Rows dbo.RowsType READONLY AS SELECT COUNT_BIG(*) AS tvp_original_header_probe FROM @Rows r JOIN dbo.Items i ON i.group_id=r.id+1").await;
    batch(&mut application, &populate("wrapped_probe", 1, InputShape::RepeatedId)).await;
    let large = populate("wrapped_probe", INPUT_ROWS, InputShape::RepeatedId);
    let reused = record_run(&mut application, &pool, "wrapped_probe", &large, Some(fixture)).await;
    trace_loop_populations(&reused.plan);
    let tvp = input(&reused.plan);
    let joins = reused
        .plan
        .nodes()
        .into_iter()
        .filter(|node| {
            node.kind == NodeKind::Join
                && node.children.first().is_some_and(|outer| {
                    matches!(outer.kind, NodeKind::Compute | NodeKind::Sort)
                        && outer.children.first().is_some_and(|child| child.id == tvp.id)
                })
        })
        .collect::<Vec<_>>();
    let [join] = joins.as_slice() else { panic!("the owned native plan must actually contain the wrapped TVP join") };
    let inner = &join.children[1];
    assert_eq!(tvp.facts().access_rows.unwrap().estimated_per_execution, Some(1.0));
    assert_eq!(tvp.facts().access_rows.unwrap().actual_total, Some(INPUT_ROWS as f64));
    assert_eq!(inner.facts().serial_execution_count.as_ref().unwrap(), &INPUT_ROWS.to_string());
    assert_eq!(inner.facts().access_rows.unwrap().actual_total, Some((INPUT_ROWS * INNER_ROWS_PER_SEEK) as f64));
    let concern = culprit_core::insights::insights(&reused.plan)
        .into_iter()
        .find(|insight| insight.rule == "nested-loops")
        .expect("the actual wrapped TVP must retain actionable measured repeated-work advice");
    assert!(concern.nodes.contains(&tvp.id));
    assert!(concern.fix_sql.is_none());
    assert_eq!(concern.fix, culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows"));
    assert!(concern.detail.contains(&culprit_core::t!("insight.nested_loops.tvp.limit")));
    // An admitted fixture intervention changes compilation, not the statement or input. An accurate fresh
    // input estimate must stop this particular diagnosis even if the resulting join still does substantial work.
    batch(&mut application, "EXEC sys.sp_recompile N'dbo.wrapped_probe'").await;
    batch(&mut application, &large).await;
    let fresh = record_run(&mut application, &pool, "wrapped_probe", &large, Some(fixture)).await;
    trace_loop_populations(&fresh.plan);
    assert_ne!(reused.identity, fresh.identity);
    assert_eq!(input(&fresh.plan).facts().access_rows.unwrap().estimated_per_execution, Some(INPUT_ROWS as f64));
    assert_eq!(fresh.plan.root.act_rows, reused.plan.root.act_rows);
    assert!(!culprit_core::insights::insights(&fresh.plan).iter().any(|insight| insight.rule == "nested-loops"
        && insight.fix == culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows")));
    eprintln!(
        "owned wrapped TVP loop: same20k input; 4m inner rows; fresh input estimate1->20k; result semantics retained"
    );
    application.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires SQL Server2022+ nonce-owned loopback fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_wrapped_tvp_loop_retains_measured_work_and_fresh_compilation_clears_the_input_underestimate() {
    super::fixture::run("wrapped", compare_wrapped_loop_after_fresh_compilation).await;
}

#[tokio::test]
#[ignore = "requires SQL Server2022+ nonce-owned loopback fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_large_to_small_tvp_reuse_links_overallocation_and_fresh_compilation_changes_the_grant() {
    super::fixture::run("grant", compare_grant_after_fresh_compilation).await;
}

#[tokio::test]
#[ignore = "requires SQL Server2022+ nonce-owned loopback fixture and CULPRIT_MSSQL_SOURCE_TEST_PORT/PASSWORD"]
async fn owned_equal_tvp_input_sizes_with_different_distributions_are_selectivity_not_input_growth() {
    super::fixture::run("skew", compare_equal_input_different_distribution).await;
}
