use super::rewrite::or_across_tables;
use super::*;
use culprit_core::{PlanKind, Properties};

fn node(id: u32, op: &str, kind: NodeKind, children: Vec<PlanNode>) -> PlanNode {
    PlanNode {
        facts: Default::default(),
        id,
        op: op.into(),
        kind,
        detail: None,
        object: None,
        index: None,
        seek_predicate: None,
        predicate: None,
        est_rows: 1.0,
        est_executions: 1.0,
        act_rows: None,
        act_executions: None,
        rows_read: None,
        table_rows: None,
        subtree_cost: 0.0,
        self_cost: 0.0,
        act_time_ms: None,
        runtime: None,
        warnings: vec![],
        props: Properties::new(),
        children,
    }
}

/// An app list as a PostgreSQL generic plan: apps and app_shares read in full, and `a.owner_id = $1 OR s.email =
/// $2` above the join, where the 5.9K apps the scan keeps become a row or two.
fn apps_plan() -> Plan {
    let mut apps = node(7, "Seq Scan", NodeKind::Scan, vec![]);
    apps.object = Some("public.apps".into());
    apps.facts_mut().table_alias = Some("a".into());
    apps.predicate = Some("(a.deleted_at IS NULL)".into());
    (apps.est_rows, apps.rows_read, apps.table_rows) = (5_880.0, Some(6_285.0), Some(6_285.0));
    (apps.self_cost, apps.subtree_cost) = (186.43, 186.43);
    let mut shares = node(5, "Seq Scan", NodeKind::Scan, vec![]);
    shares.object = Some("public.app_shares".into());
    shares.facts_mut().table_alias = Some("s".into());
    (shares.est_rows, shares.rows_read, shares.table_rows) = (845.0, Some(845.0), Some(845.0));
    (shares.self_cost, shares.subtree_cost) = (19.45, 19.45);
    let hash = node(6, "Hash", NodeKind::Other, vec![apps]);
    let mut join = node(4, "Hash Join", NodeKind::Join, vec![shares, hash]);
    join.predicate = Some("(((a.owner_id)::text = $1) OR ((s.email)::text = $2))".into());
    (join.est_rows, join.self_cost, join.subtree_cost) = (11.0, 75.72, 281.6);
    let mut root = node(0, "Sort", NodeKind::Sort, vec![join]);
    (root.est_rows, root.self_cost, root.subtree_cost) = (11.0, 0.21, 286.19);
    Plan {
        facts: Default::default(),
        view: None,
        engine: Engine::Postgres,
        kind: PlanKind::Generic,
        statement: None,
        total_cost: 286.19,
        root,
        warnings: vec![],
        index_hints: vec![],
        props: Properties::new(),
        raw: None,
        insights: None,
    }
}

const APPS_QUERY: &str = "SELECT a.id, a.name FROM apps a LEFT JOIN app_shares s ON s.app_id = a.id WHERE (a.owner_id = $1 OR s.email = $2) AND a.deleted_at IS NULL GROUP BY a.id, a.name ORDER BY a.name";

fn postgres_demo() -> Snapshot {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/postgres-demo.json")))
        .expect("fixture")
}

/// An index of the demo's on `public.<table>` (`columns`), as the catalog would list it.
fn index(snapshot: &Snapshot, table: &str, name: &str, columns: &[&str]) -> culprit_core::IndexStat {
    let mut index = snapshot.indexes[0].clone();
    (index.schema, index.table, index.name) = ("public".into(), table.into(), name.into());
    index.columns = columns.iter().map(|c| c.to_string()).collect();
    (index.include, index.filter, index.is_disabled) = (vec![], None, false);
    index
}

/// The fix for the scan of apps, for runs that return `rows` each.
fn scan_fix_on(snapshot: &Snapshot, rows: f64) -> Fix {
    let plan = apps_plan();
    let mut q = snapshot.workload.as_ref().expect("workload").queries[0].clone();
    q.text = APPS_QUERY.into();
    q.rows = Some(rows * q.executions);
    q.plan = Some(plan.clone());
    let scan = plan.nodes().into_iter().find(|n| n.id == 7).expect("the scan of apps");
    let (remedy, _) = heavy_remedy(snapshot, Some((&plan, scan)), None, &q, statement_kind(&q.text), false);
    remedy.fix(snapshot, &q)
}

fn scan_fix(rows: f64) -> String {
    scan_fix_on(&postgres_demo(), rows).summary
}

#[test]
fn a_scan_narrowed_later_is_not_too_many_rows_for_an_index() {
    // The scan keeps 94% of apps, but a run returns two rows: the OR above the join narrows them.
    let fix = scan_fix(2.0);
    assert!(!fix.contains("too many for an index"), "{fix}");
    assert!(fix.contains("returns about 2 rows a run: they are narrowed later in the plan"), "{fix}");
    assert!(fix.contains("OR spans different tables") && fix.contains("UNION rewrite"), "{fix}");
    // Runs that return about what the scan keeps need those rows: no index serves them.
    let fix = scan_fix(5_000.0);
    assert!(fix.contains("keeps about 94% of the rows, too many for an index to help"), "{fix}");
    // The aptabase-us reading: "returns about 3 rows a run" beside 2.5 in the evidence. Rounded as it is.
    assert!(scan_fix(2.5).contains("the statement returns about 2.5 rows a run"), "{}", scan_fix(2.5));
    assert!(scan_fix(1.96).contains("the statement returns about 2 rows a run"), "{}", scan_fix(1.96));
}

// The fresh PostgreSQL demo: `SELECT … FOR UPDATE` that waited 38 s for a row lock was a read, told to read fewer
// rows. A read that locks what it finds waits as a write does.
#[test]
fn rewrite_observation_is_shared_without_premature_index_sql() {
    let s = postgres_demo();
    let mut q = s.queries()[0].clone();
    q.text = APPS_QUERY.into();
    q.executions = 1000.0;
    q.rows = Some(2000.0);
    q.total_time_ms = 1.0;
    q.mean_time_ms = 0.001;
    q.plan = Some(apps_plan());
    let p = q.plan.as_ref().unwrap();
    let insight = crate::query_plan_insights(&s, &q, p).items.into_iter().find(|i| i.rule == "cross-table-or").unwrap();
    assert_eq!(insight.area, culprit_core::insights::InsightArea::Query);
    assert_eq!(insight.severity, Severity::Low);
    assert!(insight.fix_sql.is_none());
    assert!(insight.fix.contains("permission checks") && insight.verify.contains("selectivity"));
    let f = render(&s, &q, &decide(&s, &q));
    assert_eq!(f.actionability, culprit_core::Actionability::Information);
    assert!(f.fix.unwrap().sql.is_none());
    assert_eq!(f.queries[0].plan_node, insight.nodes.first().copied());
    for text in [format!("{APPS_QUERY} FOR UPDATE"), "UPDATE apps SET name = $1".into()] {
        let mut writes = q.clone();
        writes.text = text;
        assert!(
            late_filter_rewrite(&s, &writes, p).is_none(),
            "read rewrites do not change locking or write semantics"
        );
    }
    let mut with_query = s.clone();
    with_query.workload.as_mut().unwrap().queries = vec![q.clone()];
    let report = crate::analyze(with_query);
    let advisor = crate::index_advice(&report);
    assert!(advisor.notes.iter().any(|note| note.contains("Query rewrites to evaluate") && note.contains(&q.id)));
    // Larger tables keep the structural concern, without claiming that a rewrite has measured benefit.
    let mut large = p.clone();
    large.root.children[0].children[1].children[0].table_rows = Some(6_285_000.0);
    large.root.children[0].children[1].children[0].rows_read = Some(6_285_000.0);
    large.root.children[0].children[1].children[0].est_rows = 5_880_000.0;
    assert!(late_filter_rewrite(&s, &q, &large).is_some());
    let mut without_rows = q.clone();
    without_rows.rows = None;
    let observation = late_filter_rewrite(&s, &without_rows, &large).unwrap();
    assert!(!observation.fix.contains("statement returns"), "no invented row count");
    // Engine-owned objects are never candidates for a rewrite or index change.
    for n in &mut large.root.children[0].children {
        n.object = Some("pg_catalog.pg_class".into());
        for c in &mut n.children {
            c.object = Some("pg_catalog.pg_class".into());
        }
    }
    assert!(late_filter_rewrite(&s, &q, &large).is_none());
}

#[test]
fn a_read_that_locks_its_rows_is_a_locking_statement() {
    for text in [
        "SELECT id FROM orders WHERE id = $1 FOR UPDATE",
        "SELECT id FROM jobs WHERE state = $1 LIMIT $2 FOR UPDATE SKIP LOCKED",
        "SELECT id FROM orders WHERE id = $1 FOR NO KEY UPDATE",
        "SELECT * FROM accounts WHERE id = ? LOCK IN SHARE MODE",
        "SELECT [id] FROM [shop].[orders] WITH (UPDLOCK, ROWLOCK) WHERE [id] = @P1",
    ] {
        assert_eq!(statement_kind(text), StatementKind::Locking, "{text}");
    }
    for (text, kind) in [
        ("SELECT updated_for, for_update FROM t WHERE x = $1", StatementKind::Read),
        ("SELECT count(*) FROM orders WHERE status = $1", StatementKind::Aggregate),
        (
            "WITH b AS (SELECT id FROM jobs LIMIT 10) DELETE FROM jobs USING b WHERE jobs.id = b.id",
            StatementKind::Write,
        ),
    ] {
        assert_eq!(statement_kind(text), kind, "{text}");
    }
    // A plan that locks the rows it returns says so whatever the text shows.
    let mut q = postgres_demo().workload.unwrap().queries[0].clone();
    q.text = "SELECT id FROM orders WHERE id = $1".into();
    let mut plan = apps_plan();
    plan.root.op = "LockRows".into();
    q.plan = Some(plan);
    assert_eq!(kind_of(&q), StatementKind::Locking);
}

#[test]
fn each_side_of_a_union_names_the_index_it_has_or_the_one_it_needs() {
    // apps has an index on owner_id; app_shares' key leads with app_id, so the email side has none.
    let mut snapshot = postgres_demo();
    let owner = index(&snapshot, "apps", "idx_apps_owner", &["owner_id"]);
    let key = index(&snapshot, "app_shares", "PK_app_shares", &["app_id", "email"]);
    snapshot.indexes.extend([owner, key]);
    let fix = scan_fix_on(&snapshot, 2.0);
    assert!(!fix.summary.contains("an index of its own"), "{}", fix.summary);
    assert!(fix.summary.contains("idx_apps_owner on public.apps leads with owner_id"), "{}", fix.summary);
    assert!(fix.summary.contains("public.app_shares has no index that leads with email"), "{}", fix.summary);
    assert!(fix.sql.is_none(), "measure branch selectivity after a rewrite before proposing DDL");
    assert!(fix.summary.contains("Preserve the original joins, permission checks"));
    let mut unavailable = snapshot.clone();
    unavailable.sources.retain(|o| o.source != culprit_core::source_id::INDEXES);
    unavailable.sources.push(culprit_core::SourceOutcome {
        reason_message: None,
        source: culprit_core::source_id::INDEXES.into(),
        state: culprit_core::SourceState::Unavailable,
        reason: Some("permission denied for pg_index".into()),
    });
    let unknown = scan_fix_on(&unavailable, 2.0);
    assert!(!unknown.summary.contains("has no index"), "unavailable definitions do not prove absence");
    // With an index on email too, each side names its own and there is nothing to create.
    snapshot.indexes.push(index(&snapshot, "app_shares", "app_shares_email_idx", &["email"]));
    let fix = scan_fix_on(&snapshot, 2.0);
    assert!(fix.summary.contains("app_shares_email_idx on public.app_shares leads with email"), "{}", fix.summary);
    assert!(fix.sql.is_none(), "{:?}", fix.sql);
}

#[test]
fn a_whole_table_read_under_the_cost_bar_gets_the_scan_steps_advice() {
    // Costed at 177, under the 250 a step's share needs, yet not cheap: it reads all of apps on every run. The fix
    // comes from that scan (the OR as a UNION), not "kept up to date instead of recomputed" for a 2-row lookup.
    let mut snapshot = postgres_demo();
    snapshot.indexes.push(index(&snapshot, "apps", "idx_apps_owner", &["owner_id"]));
    let mut plan = apps_plan();
    (plan.total_cost, plan.root.subtree_cost) = (177.44, 177.44);
    let q = &mut snapshot.workload.as_mut().expect("workload").queries[0];
    q.text = APPS_QUERY.into();
    (q.mean_time_ms, q.executions) = (1.44, q.total_time_ms / 1.44);
    (q.rows, q.logical_reads) = (Some(2.5 * q.executions), Some(60.0 * q.executions));
    q.plan = Some(plan);
    let id = q.id.clone();
    let f = heavy_query(&snapshot).into_iter().find(|f| f.queries[0].id == id).expect("the heavy query");
    let fix = f.fix.expect("a fix");
    assert!(fix.summary.contains("UNION rewrite"), "{}", fix.summary);
    assert!(!fix.summary.contains("kept up to date"), "{}", fix.summary);
    assert!(!f.summary.contains("of its estimated cost"), "no share under the bar: {}", f.summary);
    // The summary names the scan the fix is about.
    assert!(
        f.summary.ends_with(
            "Every run includes a seq scan of public.apps that reads 6.3K rows: a scan reads the whole table, and costs more as the table grows."
        ),
        "{}",
        f.summary
    );
    assert_eq!(f.queries[0].plan_node, Some(7), "the step the fix is about");
}

#[test]
fn a_plan_that_reads_a_whole_table_is_not_called_cheap() {
    // Costed under what reading 10K rows costs, a millisecond a run and ten pages per row: cheap only while no
    // step reads a table in full, which costs more as the table grows.
    let snapshot = postgres_demo();
    let mut plan = apps_plan();
    (plan.total_cost, plan.root.subtree_cost) = (100.0, 100.0);
    let mut q = snapshot.workload.as_ref().expect("workload").queries[0].clone();
    (q.mean_time_ms, q.logical_reads, q.rows) = (1.0, Some(20.0 * q.executions), Some(2.0 * q.executions));
    assert!(!culprit_core::insights::runs_cheaply(&plan, &q));
    // The same plan finding its rows through indexes.
    fn seek(n: &mut PlanNode) {
        if n.kind == NodeKind::Scan {
            n.kind = NodeKind::Seek;
        }
        n.children.iter_mut().for_each(seek);
    }
    seek(&mut plan.root);
    assert!(culprit_core::insights::runs_cheaply(&plan, &q));
}

#[test]
fn an_or_across_tables_is_told_from_one_within_a_table() {
    assert!(or_across_tables("(((a.owner_id)::text = $1) OR ((s.email)::text = $2))"));
    assert!(or_across_tables(
        "[db].[app].[messages].[tenant_id]=[@par0] OR [db].[app].[conversations].[account_id]=[@par1]"
    ));
    assert!(!or_across_tables("((a.status = 1) OR (a.status = 2)) AND b.id = a.owner_id"));
    assert!(!or_across_tables("(s.label = 'black or white') AND (a.id = s.app_id)"));
    assert!(!or_across_tables("(a.deleted_at IS NULL)"));
}

#[test]
fn a_pools_statements_get_the_pools_own_lever() {
    // PostgreSQL's DISCARD ALL is the pool's reset: its setting, not transactions or pings.
    let discard = session_fix(Engine::Postgres, "DISCARD ALL");
    assert!(
        discard.contains("do not identify its caller or pooling mode") && discard.contains("Keep resets"),
        "{discard}"
    );
    assert!(!discard.contains("sp_reset_connection"), "{discard}");
    assert!(discard.contains("Npgsql") && discard.contains("PgBouncer") && discard.contains("benchmark"), "{discard}");
    for engine in [Engine::Postgres, Engine::Yugabyte] {
        for reset in ["DISCARD ALL", "RESET ALL", "/* pooled */ DISCARD ALL"] {
            let advice = session_fix(engine, reset);
            assert!(advice.contains("Keep resets") && advice.contains("do not identify its caller"), "{advice}");
            assert!(!advice.contains("sp_reset_connection"), "{advice}");
        }
    }
    // sp_reset_connection is SQL Server's.
    let set = session_fix(Engine::Mssql, "SET TRANSACTION ISOLATION LEVEL READ COMMITTED");
    assert!(set.contains("sp_reset_connection"), "{set}");
    let commit = session_fix(Engine::Postgres, "COMMIT");
    assert!(commit.contains("transaction of its own") && !commit.contains("sp_reset_connection"), "{commit}");
    let ping = session_fix(Engine::Mysql, "/* ping */ SELECT 1");
    assert!(ping.contains("still alive"), "{ping}");
}

#[test]
fn write_row_labels_use_the_declared_counter_meaning() {
    for (engine, expected) in [
        (Engine::Postgres, Some("Rows changed per run")),
        (Engine::Yugabyte, Some("Rows changed per run")),
        (Engine::Mssql, Some("Rows per run")),
        (Engine::Mysql, None),
    ] {
        let label = |kind| rows_labels(engine, kind).map(|(per_run, _)| per_run);
        assert_eq!(label(StatementKind::Write).as_deref(), expected);
        assert_eq!(label(StatementKind::Read).as_deref(), Some("Rows returned per run"));
    }
    // All runs' rows are the engine's own words in every language, not a run's with English's "per run" cut off.
    let totals = |engine| rows_labels(engine, StatementKind::Write).map(|(_, total)| total);
    assert_eq!(totals(Engine::Postgres).as_deref(), Some("Rows changed"));
    assert_eq!(totals(Engine::Mssql).as_deref(), Some("Rows"));
    culprit_core::i18n::scope("de", || {
        assert_eq!(totals(Engine::Postgres).as_deref(), Some("Geänderte Zeilen"));
        assert_ne!(totals(Engine::Mssql), rows_labels(Engine::Mssql, StatementKind::Write).map(|(per_run, _)| per_run));
    });
}

// The decision a finding is worded from, without its words.

#[test]
fn a_statement_that_sleeps_is_decided_by_its_idle_time() {
    let s = postgres_demo();
    let mut q = s.queries()[0].clone();
    q.text = "SELECT pg_sleep($1)".into();
    q.plan = None;
    let case = decide(&s, &q);
    assert!(matches!(&case.lead, Lead::Idle(idle) if idle.kind() == IdleKind::Sleep), "the pause leads");
    assert_eq!(case.title, TitleForm::Share, "its share, then what it does instead");
    assert!(matches!(case.remedy, Remedy::Idle(IdleKind::Sleep)));
    assert!(matches!(case.verify, Verify::Scan(Expect::TimeOrStops)));
    // A deliberate pause holds a connection, not the server's resources: listed, ranked low.
    assert_eq!(case.severity, Severity::Low);
    assert!(case.ranked_low);
    assert!(case.wait.is_none() && case.plan_node.is_none());
}

#[test]
fn a_cheap_plan_run_often_is_decided_by_how_often_it_runs() {
    // A point lookup through indexes, a millisecond a run: no step of it is the lead, and the fix cuts runs.
    let s = postgres_demo();
    let mut plan = apps_plan();
    (plan.total_cost, plan.root.subtree_cost) = (100.0, 100.0);
    fn seek(n: &mut PlanNode) {
        if n.kind == NodeKind::Scan {
            n.kind = NodeKind::Seek;
        }
        n.children.iter_mut().for_each(seek);
    }
    seek(&mut plan.root);
    let mut q = s.queries()[0].clone();
    q.text = "SELECT a.id, a.name FROM apps a WHERE a.owner_id = $1".into();
    (q.mean_time_ms, q.logical_reads, q.rows) = (1.0, Some(20.0 * q.executions), Some(2.0 * q.executions));
    q.waits.clear();
    q.plan = Some(plan);
    let case = decide(&s, &q);
    assert!(matches!(case.lead, Lead::CheapPlan { storage_reads: None }));
    assert!(matches!(case.remedy, Remedy::FewerRuns { cheap_plan: true, lever: RunsLever::Cache }));
    assert!(matches!(case.verify, Verify::Scan(Expect::FewerRuns)));
    assert_eq!(case.title, TitleForm::Share);
    assert_eq!(case.plan_node, None, "no step of a cheap plan is pointed at");
}

#[test]
fn a_late_filter_rewrite_overrides_the_remedy_and_the_step() {
    let s = postgres_demo();
    let mut q = s.queries()[0].clone();
    q.text = APPS_QUERY.into();
    (q.executions, q.rows, q.total_time_ms, q.mean_time_ms) = (1000.0, Some(2000.0), 1.0, 0.001);
    q.plan = Some(apps_plan());
    let case = decide(&s, &q);
    assert!(matches!(&case.remedy, Remedy::Rewrite(i) if i.rule == "cross-table-or"));
    assert!(matches!(&case.verify, Verify::Quoted(line) if line.contains("rewritten query")));
    assert_eq!(case.plan_node, Some(7), "the scan of apps the OR across tables narrows");
    // Index work this small is not worth acting on: the rewrite is information.
    assert_eq!(case.actionability, culprit_core::Actionability::Information);
}

// ── What each shape of finding says, word for word ───────────────────────────────────────────────────────────────
//
// Every title form, lead, wait, write, fix and verify step this check words, rendered from the demo snapshots with one
// condition planted, and pinned as English reads it: its words come from locales/en.json, and moving them there must
// not change one of them.

fn demo(name: &str) -> Snapshot {
    let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot")
}

fn query_mut<'a>(s: &'a mut Snapshot, id: &str) -> &'a mut QueryStat {
    s.workload.as_mut().expect("workload").queries.iter_mut().find(|q| q.id == id).expect("query")
}

/// The workload's total time as its statements add up, after one was changed.
fn retotal(s: &mut Snapshot) {
    let w = s.workload.as_mut().expect("workload");
    w.total_time_ms = w.queries.iter().map(|q| q.total_time_ms).sum();
}

/// Statement `id`'s finding, decided and worded, whether or not it is among the heaviest.
fn worded(s: &Snapshot, id: &str) -> Finding {
    let q = s.queries().iter().find(|q| q.id == id).expect("query");
    render(s, q, &decide(s, q))
}

/// Everything a finding says in words: its title, summary, impact, metric labels (and worded values), fix, cost,
/// verify steps and caveats, one per line.
fn said(f: &Finding) -> String {
    let mut out =
        vec![format!("title: {}", f.title), format!("summary: {}", f.summary), format!("impact: {}", f.impact.label)];
    for m in &f.metrics {
        out.push(match &m.text {
            Some(text) => format!("metric: {}: {text}", m.label),
            None => format!("metric: {}", m.label),
        });
    }
    if let Some(fix) = &f.fix {
        out.push(format!("fix: {}", fix.summary));
        if let Some(cost) = &fix.cost {
            out.push(format!("cost: {cost}"));
        }
    }
    out.extend(f.verify.iter().map(|v| format!("verify: {v}")));
    out.extend(f.caveats.iter().map(|c| format!("caveat: {c}")));
    out.join("\n")
}

fn mssql_demo() -> Snapshot {
    demo("mssql-demo.json")
}

fn mysql_demo() -> Snapshot {
    demo("mysql-demo.json")
}

/// The SQL Server demo's query 7 as a lookup on a unique index, run 3,000 times at a millisecond and returning its row
/// each time: a seek and a key lookup of `looked_up` rows each in a table of 247K rows, at `cost` in all.
fn point_lookup(looked_up: f64, cost: f64) -> Snapshot {
    let mut s = mssql_demo();
    let q = query_mut(&mut s, "7");
    (q.executions, q.mean_time_ms, q.total_time_ms, q.rows) = (3_000.0, 1.0, 3_000.0, Some(3_000.0));
    q.logical_reads = Some(6.2 * 3_000.0);
    let plan = q.plan.as_mut().expect("plan");
    plan.warnings.clear();
    plan.props.remove("PlanAffectingConvert");
    (plan.total_cost, plan.root.subtree_cost, plan.root.self_cost) = (cost, cost, 0.0);
    let (seek, lookup) = plan.root.children.split_at_mut(1);
    let (seek, lookup) = (&mut seek[0], &mut lookup[0]);
    (seek.op, seek.kind, seek.predicate) = ("Index Seek".into(), NodeKind::Seek, None);
    (seek.est_rows, seek.rows_read, seek.table_rows) = (looked_up, Some(looked_up), Some(247_435.0));
    (seek.self_cost, seek.subtree_cost) = (cost * 0.4, cost * 0.4);
    (lookup.op, lookup.kind, lookup.object) = ("Key Lookup".into(), NodeKind::Lookup, Some("shop.customers".into()));
    (lookup.est_rows, lookup.est_executions, lookup.rows_read) = (1.0, looked_up, None);
    lookup.table_rows = Some(247_435.0);
    (lookup.self_cost, lookup.subtree_cost) = (cost * 0.6, cost * 0.6);
    for n in [seek, lookup] {
        n.warnings.clear();
    }
    retotal(&mut s);
    s
}

/// [`point_lookup`] with query 7 changed by `change`.
fn point_lookup_with(change: impl FnOnce(&mut QueryStat)) -> Snapshot {
    let mut s = point_lookup(1.0, 0.0066);
    change(query_mut(&mut s, "7"));
    retotal(&mut s);
    s
}

fn wait(category: &str, wait_type: &str, ms: f64) -> culprit_core::WaitStat {
    culprit_core::WaitStat { category: category.into(), wait_type: Some(wait_type.into()), wait_ms: ms, tasks: None }
}

/// A PostgreSQL plan that inserts the values it is given into `object`.
fn insert_plan_into(object: &str) -> Plan {
    let (plan_facts, node_facts) = (
        serde_json::to_value(culprit_core::PlanFacts::default()).unwrap(),
        serde_json::to_value(culprit_core::PlanNodeFacts::default()).unwrap(),
    );
    serde_json::from_value(serde_json::json!({
        "engine": "postgres", "kind": "generic", "total_cost": 0.01, "warnings": [], "index_hints": [], "props": {},
        "facts": plan_facts,
        "root": {
            "id": 0, "op": "ModifyTable", "kind": "modify", "object": object, "detail": "Insert",
            "est_rows": 0.0, "est_executions": 1.0, "subtree_cost": 0.01, "self_cost": 0.0,
            "warnings": [], "props": {"Conflict Resolution": "NOTHING"}, "facts": node_facts.clone(),
            "children": [{
                "id": 1, "op": "Result", "kind": "compute", "est_rows": 1.0, "est_executions": 1.0,
                "subtree_cost": 0.01, "self_cost": 0.01, "warnings": [], "props": {}, "facts": node_facts,
                "children": []
            }]
        }
    }))
    .expect("plan")
}

/// A plan that sorts 400K rows of orders: the sort, the scan and a grouping between them cost `costs` (in that
/// order). MySQL puts no cost on a grouping it gives none.
fn sort_plan(engine: Engine, [sort, scan, group]: [f64; 3]) -> Plan {
    let node_facts = serde_json::to_value(culprit_core::PlanNodeFacts::default()).unwrap();
    let step = |id: u32,
                op: &str,
                kind: &str,
                object: Option<&str>,
                cost: f64,
                below: f64,
                children: serde_json::Value| {
        serde_json::json!({
            "id": id, "op": op, "kind": kind, "object": object, "est_rows": 400_000.0, "est_executions": 1.0,
            "subtree_cost": cost + below, "self_cost": cost, "warnings": [], "props": {}, "facts": node_facts.clone(),
            "children": children
        })
    };
    let scanned = step(2, "Seq Scan", "scan", Some("public.orders"), scan, 0.0, serde_json::json!([]));
    let grouped = step(1, "Aggregate", "aggregate", None, group, scan, serde_json::json!([scanned]));
    let root = step(0, "Sort", "sort", None, sort, scan + group, serde_json::json!([grouped]));
    serde_json::from_value(serde_json::json!({
        "engine": engine, "kind": "estimated", "total_cost": sort + scan + group, "warnings": [], "index_hints": [],
        "props": {}, "facts": serde_json::to_value(culprit_core::PlanFacts::default()).unwrap(), "root": root
    }))
    .expect("plan")
}

const INSERT_ID: &str = "3432126458605143571";

/// The PostgreSQL demo's lookup of customers turned into `text`, an insert into products (2,000 runs at 5 ms, 15 pages
/// each, `rows` rows changed in all, 1.7 KB of WAL a run), counted for 40 days.
fn heavy_insert(text: &str, rows: Option<f64>) -> Snapshot {
    let mut s = postgres_demo();
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(24.0 * 40.0);
    let q = query_mut(&mut s, INSERT_ID);
    q.text = text.into();
    q.plan = Some(insert_plan_into("public.products"));
    (q.executions, q.mean_time_ms, q.total_time_ms) = (2_000.0, 5.0, 10_000.0);
    (q.rows, q.logical_reads) = (rows, Some(15.0 * 2_000.0));
    q.costs = Some(culprit_core::QueryCosts {
        log_bytes: Some(2_000.0 * 1_700.0),
        log_share: Some(0.99),
        log_full_pages: Some(1_350.0),
        ..Default::default()
    });
    retotal(&mut s);
    s
}

/// `s` with statement `id` quiet for the last three of six days of statistics: it stopped.
fn stopped(mut s: Snapshot, id: &str) -> Snapshot {
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-10-05T11:54:21Z".into()), 144.0);
    let q = query_mut(&mut s, id);
    (q.first_seen, q.last_seen, q.trend) =
        (Some("2026-10-06T00:00:00Z".into()), Some("2026-10-08T00:00:00Z".into()), None);
    s
}

/// `s` as if its statistics had counted for 40 days, with 36 hours of Culprit's own scans in three steps of 10 s of
/// database time each, in which statement `id` ran `runs` times in `time_ms` a step.
fn with_scans_of(mut s: Snapshot, id: &str, runs: f64, time_ms: f64) -> Snapshot {
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-09-01T11:54:21Z".into()), 40.0 * 24.0);
    w.timeline = Some(
        serde_json::from_value(serde_json::json!({
            "source": "scans",
            "starts": ["2026-10-10T00:00:00Z", "2026-10-10T12:00:00Z", "2026-10-11T00:00:00Z"],
            "end": "2026-10-11T12:00:00Z",
            "time_ms": [10000.0, 10000.0, 10000.0],
            "executions": [5000.0, 5000.0, 5000.0]
        }))
        .expect("timeline"),
    );
    query_mut(&mut s, id).trend =
        Some(culprit_core::QueryTrend { time_ms: vec![Some(time_ms); 3], executions: vec![Some(runs); 3] });
    s
}

fn with_scans(id: &str, runs: f64, time_ms: f64) -> Snapshot {
    with_scans_of(postgres_demo(), id, runs, time_ms)
}

const UPDATE_ID: &str = "4911431806993420100";
const SLEEP_ID: &str = "-8886085649470066503";

/// The PostgreSQL demo's update with a 19 s recorded duration, as `text`, with `change` made to it.
fn pg_update_with(text: &str, change: impl FnOnce(&mut QueryStat)) -> Snapshot {
    let mut s = postgres_demo();
    let q = query_mut(&mut s, UPDATE_ID);
    q.text = text.into();
    change(q);
    retotal(&mut s);
    s
}

/// The SQL Server demo's query 10 run at 900 ms, 2% of it on CPU, with one Query Store wait of `share` of its time.
fn query_store_wait(category: &str, wait_type: &str, share: f64) -> Snapshot {
    let mut s = mssql_demo();
    let q = query_mut(&mut s, "10");
    (q.mean_time_ms, q.total_time_ms) = (900.0, 900.0 * q.executions);
    q.cpu_time_ms = Some(q.total_time_ms * 0.02);
    q.waits = vec![wait(category, wait_type, q.total_time_ms * share)];
    retotal(&mut s);
    s
}

const MYSQL_LOOKUP: &str = "9b30e6ad79d940fe";

/// The MySQL demo with statement `id` changed by `change`.
fn mysql_with(id: &str, change: impl FnOnce(&mut QueryStat)) -> Snapshot {
    let mut s = mysql_demo();
    change(query_mut(&mut s, id));
    retotal(&mut s);
    s
}

#[test]
fn few_reads_and_missing_cpu_do_not_establish_waiting_or_a_row_lock_cause() {
    for text in ["UPDATE products SET price=$1 WHERE id=$2", "SELECT id FROM products WHERE id=$1 FOR UPDATE"] {
        let snapshot = pg_update_with(text, |query| {
            query.cpu_time_ms = None;
            query.waits.clear();
            query.spread = None;
            query.logical_reads = Some(query.executions);
            query.plan = None;
        });
        let query = snapshot.queries().iter().find(|query| query.id == UPDATE_ID).unwrap();
        assert!(idle_time(&snapshot, query).is_none(), "neither SQL shape nor read counts measure waiting");
        assert!(!matches!(decide(&snapshot, query).lead, Lead::Idle(_)));
    }
    let snapshot = mysql_with(MYSQL_LOOKUP, |query| {
        query.text = "UPDATE orders SET total=? WHERE id=?".into();
        query.mean_time_ms = 2000.0;
        query.total_time_ms = query.mean_time_ms * query.executions;
        query.cpu_time_ms = None;
        query.logical_reads = Some(query.executions);
        query.waits.clear();
        query.spread = Some(culprit_core::Spread { lock_ms: Some(0.0), ..Default::default() });
        query.plan = None;
    });
    let query = snapshot.queries().iter().find(|query| query.id == MYSQL_LOOKUP).unwrap();
    assert!(idle_time(&snapshot, query).is_none(), "a native zero lock timer does not prove other waiting");
    assert!(!matches!(decide(&snapshot, query).lead, Lead::Idle(_)));
}

#[test]
fn recorded_waits_and_recorded_low_cpu_remain_independent_positive_evidence() {
    let mut snapshot = query_store_wait("Lock", "Lock", 0.8);
    query_mut(&mut snapshot, "10").cpu_time_ms = None;
    let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
    assert!(matches!(decide(&snapshot, query).wait, Some(DominantWait { on: WaitOn::Locks, .. })));

    let mut snapshot = query_store_wait("CPU", "CPU", 0.02);
    let query = query_mut(&mut snapshot, "10");
    query.waits.clear();
    query.logical_reads = Some(5.0 * query.executions);
    let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
    assert!(matches!(idle_time(&snapshot, query).map(|idle| idle.kind()), Some(IdleKind::Unknown)));

    let snapshot = mysql_with(MYSQL_LOOKUP, |query| {
        query.mean_time_ms = 2000.0;
        query.total_time_ms = query.mean_time_ms * query.executions;
        query.cpu_time_ms = None;
        query.spread = Some(culprit_core::Spread { lock_ms: Some(query.total_time_ms * 0.8), ..Default::default() });
    });
    let query = snapshot.queries().iter().find(|query| query.id == MYSQL_LOOKUP).unwrap();
    assert!(matches!(idle_time(&snapshot, query).map(|idle| idle.kind()), Some(IdleKind::Locks)));
}

#[test]
fn embedded_native_timers_and_declared_read_units_do_not_require_a_provider_identity() {
    for engine in Engine::ALL {
        let mut snapshot = query_store_wait("Lock", "Native.ParallelLock", 2.0);
        snapshot.server.engine = engine;
        let query = query_mut(&mut snapshot, "10");
        query.cpu_time_ms = None;
        query.spread = Some(culprit_core::Spread { lock_ms: Some(query.total_time_ms * 0.8), ..Default::default() });
        let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
        let scalar = idle_time(&snapshot, query).expect("an embedding source supplied an explicit native lock timer");
        assert_eq!(scalar.kind(), IdleKind::Locks);
        let evidence = scalar.evidence(&snapshot, query);
        assert!(evidence.contains(&format::duration(query.spread.as_ref().unwrap().lock_ms.unwrap())), "{evidence}");
        assert!(!evidence.contains('%'), "a scalar timer does not qualify an elapsed share: {evidence}");
        assert_eq!(scalar.verify(), Expect::RecordedWait);

        let query = query_mut(&mut snapshot, "10");
        query.spread = None;
        let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
        assert!(idle_time(&snapshot, query).is_none(), "worker wait totals cannot become a scalar lock timer");
        let material = dominant_wait(query).unwrap().sentence(&snapshot, query);
        assert!(
            material.contains("Native.ParallelLock") && material.contains(&format::duration(query.waits[0].wait_ms))
        );

        let query = query_mut(&mut snapshot, "10");
        query.cpu_time_ms = Some(query.total_time_ms * 0.02);
        let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
        let recorded = idle_time(&snapshot, query).unwrap();
        assert_eq!(recorded.verify(), Expect::RecordedWait, "the vector remains recorded wait evidence");

        let query = query_mut(&mut snapshot, "10");
        query.waits.clear();
        query.logical_reads = Some(query.executions);
        let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
        let evidence = idle_time(&snapshot, query).unwrap().evidence(&snapshot, query);
        let verify = verify_line(&snapshot, query, Expect::Measure);
        match engine.spec().statement_metrics().reads {
            ReadUnit::RowsExamined => {
                assert!(evidence.contains("1 row examined per run"), "{evidence}");
                assert!(verify.contains("rows examined"), "{verify}");
            }
            ReadUnit::Pages => {
                assert!(evidence.contains("1 page per run"), "{evidence}");
                assert!(verify.contains("pages read"), "{verify}");
            }
        }
    }
}

#[test]
fn recorded_waits_remain_totals_even_when_parallel_waits_exceed_execution_time() {
    for wait_ms in [250.0, 750.0, 2500.0] {
        let mut snapshot = query_store_wait("Lock", "Native.SchemaLock", 0.8);
        let query = query_mut(&mut snapshot, "10");
        query.executions = 5.0;
        query.mean_time_ms = 200.0;
        query.total_time_ms = 1000.0;
        query.cpu_time_ms = Some(20.0);
        query.waits[0].wait_ms = wait_ms;
        query.waits_coverage = Some(culprit_core::QueryWaitCoverage {
            classes: vec![culprit_core::telemetry::WaitClass::Lock],
            complete: false,
        });
        let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
        let idle = idle_time(&snapshot, query).expect("partial recorded waits remain positive evidence");
        assert_eq!(idle.kind(), IdleKind::Locks);
        assert_eq!(idle.verify(), Expect::RecordedWait);
        let title = idle.title_tail(snapshot.server.engine);
        let evidence = idle.evidence(&snapshot, query);
        for text in [&title, &evidence] {
            assert!(text.contains(&format::duration(wait_ms)), "the native total must not be clipped: {text}");
            assert!(text.contains("Native.SchemaLock"), "{text}");
            assert!(!text.contains('%') && !text.contains("most of"), "not an exclusive elapsed share: {text}");
        }
        assert!(evidence.contains("measured CPU time is 20 ms"), "{evidence}");
        let verify = verify_line(&snapshot, query, idle.verify());
        assert!(verify.contains("equivalent useful work, parameters and load"), "{verify}");
        assert!(!verify.contains("toward what its reads take"), "{verify}");
        let fix = idle.kind().fix(&snapshot, query).summary;
        assert!(fix.contains("lock resource"), "a schema lock is not proof of a row-lock cause: {fix}");
        assert!(!fix.contains("won't shorten"), "{fix}");
    }
}

#[test]
fn material_io_waits_do_not_prove_a_cache_miss_or_an_elapsed_share() {
    let mut snapshot = query_store_wait("IO", "Native.TableHandler", 2.5);
    let query = query_mut(&mut snapshot, "10");
    query.waits_coverage = None;
    let wait_ms = query.waits[0].wait_ms;
    // An opaque, larger category does not erase a recognized positive I/O observation.
    query.waits.push(wait("FutureNativeCategory", "unknown-native", wait_ms * 2.0));
    let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
    let material = dominant_wait(query).unwrap();
    assert_eq!(material.on, WaitOn::Storage);
    let sentence = material.sentence(&snapshot, query);
    assert!(sentence.contains("Native.TableHandler") && sentence.contains(&format::duration(wait_ms)), "{sentence}");
    assert!(!sentence.contains('%') && !sentence.contains("most of"), "{sentence}");
    assert!(!sentence.contains("cache") && !sentence.contains("wasn't in memory"), "{sentence}");
    let verify = verify_line(&snapshot, query, Expect::Storage);
    assert!(verify.contains("I/O measurements and execution time"), "{verify}");
    assert!(!verify.contains("elapsed time"), "{verify}");
}

#[test]
fn invalid_wait_totals_do_not_become_material_positive_evidence() {
    for wait_ms in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0] {
        let mut snapshot = query_store_wait("IO", "native-io", 0.8);
        let query = query_mut(&mut snapshot, "10");
        query.waits[0].wait_ms = wait_ms;
        assert!(dominant_wait(query).is_none(), "invalid recorded wait: {wait_ms}");
    }
}

#[test]
fn portable_wait_classes_use_declared_source_wording_and_localized_names_without_native_types() {
    culprit_core::i18n::scope("es", || {
        let expected = culprit_core::t!("core.waits.category.network.label");
        for engine in Engine::ALL {
            let mut snapshot = query_store_wait("Network", "native-client-event", 0.8);
            snapshot.server.engine = engine;
            let workload = snapshot.workload.as_mut().unwrap();
            workload.kind = None;
            workload.source = "custom-native-history".into();
            let query = workload.queries.iter_mut().find(|query| query.id == "10").unwrap();
            query.waits[0].wait_type = None;
            let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
            let idle = idle_time(&snapshot, query).expect("the declared native Network wait remains positive evidence");
            assert_eq!(idle.kind(), IdleKind::Client);
            let title = idle.title_tail(engine);
            assert!(title.contains(&expected) && !title.contains("Network"), "{title}");
            let evidence = idle.evidence(&snapshot, query);
            assert!(evidence.contains(&expected), "{evidence}");
            assert!(!evidence.contains("Query Store"), "{evidence}");
            assert!(!evidence.contains("Network"), "portable English category must not leak: {evidence}");
            let dominant = dominant_wait(query).unwrap().sentence(&snapshot, query);
            assert!(dominant.contains(&expected), "{dominant}");
            assert!(!dominant.contains("Network"), "{dominant}");
        }
    });
}

#[test]
fn actual_native_wait_names_remain_unchanged_when_findings_are_localized() {
    culprit_core::i18n::scope("de", || {
        let snapshot = query_store_wait("Network", "Native.ClientWait_17", 0.8);
        let query = snapshot.queries().iter().find(|query| query.id == "10").unwrap();
        let idle = idle_time(&snapshot, query).unwrap();
        let evidence = idle.evidence(&snapshot, query);
        let title = idle.title_tail(snapshot.server.engine);
        let dominant = dominant_wait(query).unwrap().sentence(&snapshot, query);
        assert!(evidence.contains("Native.ClientWait_17"), "{evidence}");
        assert!(title.contains("Native.ClientWait_17"), "{title}");
        assert!(dominant.contains("Native.ClientWait_17"), "{dominant}");
    });
}

/// `s` with these triggers on products (`name:events`).
fn with_triggers(mut s: Snapshot, triggers: &[&str]) -> Snapshot {
    let products = s.tables.iter_mut().find(|t| t.name == "products").expect("products");
    products.storage = Some(culprit_core::TableStorage {
        options: triggers.iter().map(|t| format!("trigger={t}")).collect(),
        ..Default::default()
    });
    s
}

fn slow_log(mut s: Snapshot) -> Snapshot {
    let w = s.workload.as_mut().unwrap();
    w.source = "slow query log".into();
    w.kind = Some(culprit_core::WorkloadKind::SlowLog);
    w.windowed = true;
    s.settings.retain(|x| !x.name.eq_ignore_ascii_case("long_query_time"));
    s.settings.push(culprit_core::Setting {
        name: "long_query_time".into(),
        value: "0.100000".into(),
        default_value: None,
        scope: "server".into(),
    });
    s
}

#[test]
fn current_zero_logging_threshold_does_not_establish_full_database_coverage() {
    let mut snapshot = slow_log(mysql_demo());
    snapshot.workload.as_mut().unwrap().min_logged_ms = None;
    for value in ["0", "0.100000", "unknown"] {
        snapshot.settings.iter_mut().find(|setting| setting.name == "long_query_time").unwrap().value = value.into();
        assert!(slow_log_only(&snapshot), "current setting {value} cannot establish historical population");
        assert!(crate::queries::time_noun(&snapshot).contains("logged-run time"));
    }
}

/// A residual filter in an estimated plan beside many recorded pages and no returned row.
fn lookups_for_nothing() -> Snapshot {
    point_lookup_with(|q| {
        q.text = "SELECT [id], [name] FROM [shop].[customers] WHERE [email] = @P1 AND [segment] = @P2".into();
        (q.logical_reads, q.rows) = (Some(750.0 * 3_000.0), Some(0.0));
        let plan = q.plan.as_mut().expect("plan");
        plan.root.children[0].seek_predicate = Some("[email] = [@P1]".into());
        plan.root.children[1].predicate = Some("[culprit_demo].[shop].[customers].[segment]=[@P2]".into());
    })
}

/// The scenarios, by group: each a name and everything its finding says.
fn scenarios() -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut add = |name: &'static str, s: Snapshot, id: &str| out.push((name, said(&worded(&s, id))));

    // Titles.
    add("title/share", postgres_demo(), "-3722457391793582113");
    add(
        "title/share-stopped",
        stopped(
            heavy_insert("INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT DO NOTHING", Some(500.0)),
            INSERT_ID,
        ),
        INSERT_ID,
    );
    add(
        "title/share-idle-stopped",
        stopped(
            pg_update_with("UPDATE products SET name = name WHERE id = $1", |q| {
                (q.executions, q.total_time_ms, q.logical_reads) = (50.0, 50.0 * q.mean_time_ms, Some(50.0 * 15.0));
            }),
            UPDATE_ID,
        ),
        UPDATE_ID,
    );
    add(
        "title/nothing-pages",
        point_lookup_with(|q| (q.logical_reads, q.rows) = (Some(750.0 * 3_000.0), Some(0.0))),
        "7",
    );
    add(
        "title/less-than-a-row-pages",
        point_lookup_with(|q| (q.logical_reads, q.rows) = (Some(750.0 * 3_000.0), Some(900.0))),
        "7",
    );
    add(
        "title/nothing-rows-examined",
        mysql_with(MYSQL_LOOKUP, |q| (q.logical_reads, q.rows) = (Some(500.0 * 30_000.0), Some(0.0))),
        MYSQL_LOOKUP,
    );
    let mut s = postgres_demo();
    let q = query_mut(&mut s, INSERT_ID);
    (q.logical_reads, q.rows, q.total_time_ms, q.executions) = (Some(500.0 * 8_193.0), Some(0.0), 40_000.0, 8_193.0);
    retotal(&mut s);
    add("title/nothing-pages-stopped", stopped(s, INSERT_ID), INSERT_ID);
    let mut s = mysql_with(MYSQL_LOOKUP, |q| (q.logical_reads, q.rows) = (Some(500.0 * 30_000.0), Some(0.0)));
    let q = query_mut(&mut s, MYSQL_LOOKUP);
    (q.first_seen, q.last_seen) = (Some("2026-10-06T00:00:00Z".into()), Some("2026-10-08T00:00:00Z".into()));
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-10-05T12:09:00Z".into()), 144.0);
    add("title/nothing-rows-examined-stopped", s, MYSQL_LOOKUP);
    add("title/many-per-row-pages", postgres_demo(), "5308963031944094559");
    add("title/many-per-row-rows-examined", mysql_demo(), "19bd4ff468839ea3");
    add("title/many-per-row-pages-stopped", stopped(postgres_demo(), "5308963031944094559"), "5308963031944094559");

    // Where a statement's time goes when it isn't work.
    add("idle/sleep", postgres_demo(), SLEEP_ID);
    let mut s = postgres_demo();
    query_mut(&mut s, SLEEP_ID).text = "SELECT pg_advisory_lock($1)".into();
    add("idle/app-lock", s, SLEEP_ID);
    add("unknown-cause/pg-write", postgres_demo(), UPDATE_ID);
    add(
        "unknown-cause/locking-clause",
        pg_update_with("SELECT id FROM orders WHERE id = $1 FOR UPDATE", |q| {
            (q.executions, q.mean_time_ms, q.total_time_ms) = (3.0, 38_412.0, 115_236.0);
            (q.rows, q.logical_reads, q.plan) = (Some(3.0), Some(27.0), None);
        }),
        UPDATE_ID,
    );
    add(
        "unknown-cause/locking-plan",
        pg_update_with("SELECT id FROM orders WHERE id = $1", |q| {
            q.plan.as_mut().expect("plan").root.op = "LockRows".into();
        }),
        UPDATE_ID,
    );
    let mut s = mssql_demo();
    let q = query_mut(&mut s, "10");
    q.text = "UPDATE [shop].[support_tickets] SET [status] = @P1 WHERE [id] = @P2".into();
    (q.mean_time_ms, q.total_time_ms, q.cpu_time_ms) = (900.0, 900.0 * q.executions, None);
    (q.logical_reads, q.rows, q.waits) = (Some(5.0 * q.executions), Some(q.executions), vec![]);
    retotal(&mut s);
    add("unknown-cause/mssql-write", s, "10");
    let unnamed = |q: &mut QueryStat| {
        q.text = "UPDATE `orders` SET `total` = ? WHERE `id` = ?".into();
        (q.mean_time_ms, q.total_time_ms) = (2_000.0, 2_000.0 * q.executions);
        (q.logical_reads, q.rows, q.cpu_time_ms, q.plan) = (Some(10.0 * q.executions), Some(q.executions), None, None);
    };
    add("unknown-cause/mysql-write", mysql_with(MYSQL_LOOKUP, unnamed), MYSQL_LOOKUP);
    let lock_time = |q: &mut QueryStat| {
        unnamed(q);
        q.waits.clear();
        q.spread.get_or_insert_with(Default::default).lock_ms = Some(q.total_time_ms * 0.8);
    };
    add("idle/lock-time", mysql_with(MYSQL_LOOKUP, lock_time), MYSQL_LOOKUP);
    for (name, category, wait_type, share) in [
        ("idle/qs-locks-most", "Lock", "Lock", 0.9),
        ("idle/qs-locks-share", "Lock", "Lock", 0.3),
        ("idle/qs-client-most", "Network", "Network IO", 0.9),
        ("idle/qs-client-share", "Network", "Network IO", 0.3),
        ("idle/qs-memory-most", "Memory", "Memory", 0.9),
        ("idle/qs-memory-share", "Memory", "Memory", 0.3),
        ("idle/qs-latch-most", "Latch", "Buffer Latch", 0.9),
        ("idle/qs-latch-share", "Latch", "Buffer Latch", 0.3),
    ] {
        add(name, query_store_wait(category, wait_type, share), "10");
    }
    let mut s = query_store_wait("CPU", "CPU", 0.02);
    let q = query_mut(&mut s, "10");
    (q.logical_reads, q.waits) = (Some(5.0 * q.executions), vec![]);
    add("idle/little-cpu-pages", s, "10");
    add(
        "idle/little-cpu-rows-examined",
        mysql_with(MYSQL_LOOKUP, |q| {
            (q.mean_time_ms, q.total_time_ms) = (2_000.0, 2_000.0 * q.executions);
            (q.logical_reads, q.cpu_time_ms) = (Some(10.0 * q.executions), Some(0.02 * 2_000.0 * q.executions));
        }),
        MYSQL_LOOKUP,
    );
    // Figures from the totals beside a summary over Culprit's scans.
    let mut s = with_scans(UPDATE_ID, 5.0, 9_500.0);
    let q = query_mut(&mut s, UPDATE_ID);
    (q.executions, q.total_time_ms, q.logical_reads) = (40.0, 40.0 * q.mean_time_ms, Some(40.0 * 15.0));
    retotal(&mut s);
    add("unknown-cause/pg-over-totals", s, UPDATE_ID);
    let mut s = with_scans_of(mysql_with(MYSQL_LOOKUP, unnamed), MYSQL_LOOKUP, 5.0, 9_500.0);
    let q = query_mut(&mut s, MYSQL_LOOKUP);
    (q.executions, q.mean_time_ms, q.total_time_ms) = (40.0, 2_000.0, 80_000.0);
    (q.logical_reads, q.rows) = (Some(400.0), Some(40.0));
    retotal(&mut s);
    add("unknown-cause/mysql-over-totals", s.clone(), MYSQL_LOOKUP);
    let q = query_mut(&mut s, MYSQL_LOOKUP);
    q.waits.clear();
    q.spread.get_or_insert_with(Default::default).lock_ms = Some(q.total_time_ms * 0.8);
    add("idle/lock-time-over-totals", s.clone(), MYSQL_LOOKUP);
    let q = query_mut(&mut s, MYSQL_LOOKUP);
    q.text = "SELECT `id` , NAME , `price` FROM `products` WHERE `id` = ?".into();
    q.spread.as_mut().unwrap().lock_ms = None;
    (q.waits, q.cpu_time_ms) = (vec![], Some(0.02 * q.total_time_ms));
    add("idle/little-cpu-over-totals", s, MYSQL_LOOKUP);

    // What explains the time, after its share.
    add("lead/step-share-examined", mysql_demo(), "c7b5ac9bb4bcecfe");
    add("lead/uncosted-temporary", mysql_demo(), "04cd7be9c366fadc");
    let grouping = |s: &mut Snapshot| {
        let plan = query_mut(s, "04cd7be9c366fadc").plan.as_mut().expect("plan");
        plan.root.children[0].children[0].detail = None;
        plan.root.self_cost = 1.0;
    };
    let mut s = mysql_demo();
    grouping(&mut s);
    add("lead/uncosted-grouping-costliest", s, "04cd7be9c366fadc");
    let mut s = mysql_demo();
    let plan = query_mut(&mut s, "19bd4ff468839ea3").plan.as_mut().expect("plan");
    (plan.root.children[0].kind, plan.root.children[0].op, plan.root.children[0].detail) =
        (NodeKind::Spool, "Materialize".into(), None);
    add("lead/uncosted-spool", s, "19bd4ff468839ea3");
    let mut s = postgres_demo();
    let plan = query_mut(&mut s, "5308963031944094559").plan.as_mut().expect("plan");
    let hot = culprit_core::insights::working_step(plan).expect("a working step").id;
    fn guess(n: &mut PlanNode, id: u32) {
        if n.id == id {
            n.facts_mut().selectivity_guess = Some("For 'PLANTED_GUESS'".into());
        }
        n.children.iter_mut().for_each(|c| guess(c, id));
    }
    guess(&mut plan.root, hot);
    add("lead/step-costed-highest", s, "5308963031944094559");
    add("lead/lookups-looks-up", lookups_for_nothing(), "7");
    let mut s = lookups_for_nothing();
    s.server.engine = Engine::Mysql;
    query_mut(&mut s, "7").plan.as_mut().expect("plan").engine = Engine::Mysql;
    add("lead/lookups-rows-examined", s, "7");
    let mut s = lookups_for_nothing();
    query_mut(&mut s, "7").plan.as_mut().expect("plan").root.children[0].est_rows = 3.0;
    add("lead/lookups-estimates-rows", s, "7");
    let mut s = lookups_for_nothing();
    let q = query_mut(&mut s, "7");
    q.rows = Some(900.0);
    let plan = q.plan.as_mut().expect("plan");
    plan.root.children[0].predicate = plan.root.children[1].predicate.take();
    add("lead/lookups-checks-less-than-a-row", s, "7");
    add(
        "lead/cheap-write",
        point_lookup_with(|q| q.text = "INSERT INTO [shop].[customers] ([name], [email]) VALUES (@P1, @P2)".into()),
        "7",
    );
    add("lead/cheap-plan", point_lookup(1.0, 0.0066), "7");
    add(
        "lead/cheap-plan-storage-finds-nothing",
        point_lookup_with(|q| {
            q.rows = Some(1.0);
            q.waits = vec![wait("IO", "Buffer IO", q.total_time_ms * 0.86)];
        }),
        "7",
    );
    add(
        "lead/cheap-plan-storage-rows-examined",
        mysql_with(MYSQL_LOOKUP, |q| q.waits = vec![wait("IO", "wait/io/table/sql/handler", q.total_time_ms * 0.86)]),
        MYSQL_LOOKUP,
    );
    let mut s = postgres_demo();
    s.indexes.push(index(&s, "apps", "idx_apps_owner", &["owner_id"]));
    let mut plan = apps_plan();
    (plan.total_cost, plan.root.subtree_cost) = (177.44, 177.44);
    let q = &mut s.workload.as_mut().expect("workload").queries[0];
    q.text = APPS_QUERY.into();
    (q.mean_time_ms, q.executions) = (1.44, q.total_time_ms / 1.44);
    (q.rows, q.logical_reads) = (Some(2.5 * q.executions), Some(60.0 * q.executions));
    q.plan = Some(plan);
    let id = q.id.clone();
    add("lead/whole-scan-rewrite", s, &id);
    add(
        "lead/unexplained-read",
        {
            let mut s = postgres_demo();
            let q = query_mut(&mut s, INSERT_ID);
            (q.plan, q.mean_time_ms, q.total_time_ms) = (None, 5.0, 5.0 * q.executions);
            retotal(&mut s);
            s
        },
        INSERT_ID,
    );

    // The summary's first sentence.
    add("summary/recent-entry", with_scans(INSERT_ID, 2_000.0, 1_000.0), INSERT_ID);
    add("summary/scans-beside-totals", with_scans(INSERT_ID, 100.0, 1_000.0), INSERT_ID);
    let mut s = postgres_demo();
    let q = query_mut(&mut s, UPDATE_ID);
    (q.executions, q.total_time_ms, q.max_time_ms) = (2.0, 21_000.0, Some(20_000.0));
    retotal(&mut s);
    add("summary/ran-taking", s, UPDATE_ID);
    let mut s = postgres_demo();
    s.workload.as_mut().unwrap().untracked_time_ms = Some(5_000.0);
    add("summary/of-what-it-could-attribute", s, "-3722457391793582113");
    let mut s = point_lookup(1.0, 0.0066);
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (Some("2026-10-05T12:13:47Z".into()), 144.0);
    let q = query_mut(&mut s, "7");
    (q.first_seen, q.last_seen) = (Some("2026-10-11T07:44:33Z".into()), Some("2026-10-11T07:47:52Z".into()));
    add("summary/burst-counted", s, "7");
    add("summary/slow-log", slow_log(mysql_demo()), "c7b5ac9bb4bcecfe");

    // The wait that takes most of its time.
    for (name, category, wait_type, share) in [
        ("wait/storage", "IO", "Buffer IO", 0.85),
        ("wait/storage-most", "IO", "Buffer IO", 1.4),
        ("wait/locks", "Lock", "Lock", 0.85),
        ("wait/latches", "Latch", "Buffer Latch", 0.85),
        ("wait/log", "Log", "Tran Log IO", 0.85),
        ("wait/client", "Network", "Network IO", 0.85),
        ("wait/memory", "Memory", "Memory", 0.85),
        ("wait/compilation", "Compilation", "Compilation", 0.85),
    ] {
        add(name, point_lookup_with(|q| q.waits = vec![wait(category, wait_type, q.total_time_ms * share)]), "7");
    }
    let storage = |q: &mut QueryStat| {
        q.plan = None;
        q.waits = vec![wait("IO", "DataFileRead", q.total_time_ms * 0.9)];
    };
    let mut s = postgres_demo();
    storage(query_mut(&mut s, INSERT_ID));
    add("wait/storage-files-not-cheap", s.clone(), INSERT_ID);
    query_mut(&mut s, INSERT_ID).logical_reads = None;
    add("wait/storage-files-no-reads", s, INSERT_ID);
    let mut s = with_scans(INSERT_ID, 100.0, 1_000.0);
    storage(query_mut(&mut s, INSERT_ID));
    add("wait/storage-over-totals", s, INSERT_ID);
    add(
        "wait/storage-cheap-fewer-runs",
        point_lookup_with(|q| q.waits = vec![wait("IO", "Buffer IO", q.total_time_ms * 0.86)]),
        "7",
    );
    let mut s = point_lookup_with(|q| {
        q.rows = Some(1.0);
        q.waits = vec![wait("IO", "Buffer IO", q.total_time_ms * 0.86)];
    });
    let email = s.indexes.iter_mut().find(|i| i.name == "ix_customers_email").expect("ix_customers_email");
    (email.is_unique, email.filter) = (true, Some("[email] IS NOT NULL".into()));
    add("wait/storage-finds-nothing-unique-filtered", s.clone(), "7");
    s.indexes.iter_mut().find(|i| i.name == "ix_customers_email").unwrap().filter = None;
    add("wait/storage-finds-nothing-unique", s, "7");

    // Triggers, statistics maintenance.
    add("triggers/one-update", with_triggers(postgres_demo(), &["trg_audit_row:IUD"]), UPDATE_ID);
    add("triggers/many-update", with_triggers(postgres_demo(), &["trg_b:U", "trg_a:IUD", "trg_c:I"]), UPDATE_ID);
    for (name, text) in [
        ("triggers/insert", "INSERT INTO products (sku, name) VALUES ($1, $2)"),
        ("triggers/delete", "DELETE FROM products WHERE id = $1"),
        (
            "triggers/merge",
            "MERGE INTO products p USING staged s ON p.sku = s.sku WHEN MATCHED THEN UPDATE SET name = s.name",
        ),
    ] {
        add(name, with_triggers(pg_update_with(text, |_| {}), &["trg_audit_row:IUD"]), UPDATE_ID);
    }
    let mut s = mssql_demo();
    let w = s.workload.as_mut().unwrap();
    let mut q = w.queries[0].clone();
    q.id = "statistics-maintenance".into();
    q.text = "UPDATE STATISTICS dbo.orders WITH FULLSCAN".into();
    (q.object_name, q.plan, q.trend, q.rows, q.first_seen, q.last_seen) = (None, None, None, None, None, None);
    q.plans.clear();
    q.waits.clear();
    (q.executions, q.total_time_ms, q.mean_time_ms) = (2.0, 20_000.0, 10_000.0);
    (w.total_time_ms, w.total_executions) = (q.total_time_ms, q.executions);
    w.queries = vec![q];
    add("summary/statistics-maintenance", s, "statistics-maintenance");

    // Writes: what a run costs, and the levers.
    let nothing = "INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT DO NOTHING";
    let insert = "INSERT INTO products (sku, name) VALUES ($1, $2)";
    add("write/skip-insert", heavy_insert(nothing, Some(500.0)), INSERT_ID);
    add(
        "write/get-or-create",
        heavy_insert(
            "INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT (sku) DO UPDATE SET name = products.name RETURNING name",
            Some(2_000.0),
        ),
        INSERT_ID,
    );
    add(
        "write/get-or-create-changing-nothing",
        heavy_insert(
            "INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT (sku) DO UPDATE SET name = products.name RETURNING name",
            Some(500.0),
        ),
        INSERT_ID,
    );
    add(
        "write/upsert",
        heavy_insert(
            "INSERT INTO products (sku, name) VALUES ($1, $2) ON CONFLICT (sku) DO UPDATE SET name = EXCLUDED.name",
            Some(2_000.0),
        ),
        INSERT_ID,
    );
    add("write/batch-insert-unused-index", heavy_insert(insert, Some(2_000.0)), INSERT_ID);
    let mut s = heavy_insert(insert, Some(2_000.0));
    let mut unused = s.indexes.iter().find(|i| i.name == "products_sku_idx").cloned().expect("products_sku_idx");
    unused.name = "products_name_idx".into();
    s.indexes.push(unused);
    add("write/batch-insert-unused-indexes", s, INSERT_ID);
    add("write/skip-update", heavy_insert("UPDATE products SET name = $1 WHERE sku = $2", Some(500.0)), INSERT_ID);
    add(
        "write/skip-delete-stopped",
        stopped(heavy_insert("DELETE FROM products WHERE sku = $1", Some(500.0)), INSERT_ID),
        INSERT_ID,
    );
    add("write/batch-update", heavy_insert("UPDATE products SET name = $1 WHERE sku = $2", Some(2_000.0)), INSERT_ID);
    add("write/batch-delete", heavy_insert("DELETE FROM products WHERE sku = $1", Some(2_000.0)), INSERT_ID);
    add(
        "write/batch-merge",
        heavy_insert(
            "MERGE INTO products p USING staged s ON p.sku = s.sku WHEN MATCHED THEN UPDATE SET name = s.name",
            Some(2_000.0),
        ),
        INSERT_ID,
    );
    add("write/log-only", heavy_insert(insert, None), INSERT_ID);
    add("write/cheap-stopped", stopped(heavy_insert(insert, Some(2_000.0)), INSERT_ID), INSERT_ID);
    let not_cheap = || {
        let mut s = postgres_demo();
        let q = query_mut(&mut s, INSERT_ID);
        q.text = insert.into();
        (q.plan, q.executions, q.mean_time_ms, q.total_time_ms, q.rows) =
            (None, 30_000.0, 1.0, 30_000.0, Some(30_000.0));
        retotal(&mut s);
        s
    };
    add("write/not-cheap", not_cheap(), INSERT_ID);
    let mut s = stopped(not_cheap(), INSERT_ID);
    let q = query_mut(&mut s, INSERT_ID);
    (q.executions, q.total_time_ms, q.rows) = (1_000_000.0, 1_000_000.0, Some(1_000_000.0));
    retotal(&mut s);
    add("write/not-cheap-stopped", s, INSERT_ID);
    add("write/over-totals", with_scans_of(heavy_insert(nothing, Some(500.0)), INSERT_ID, 100.0, 1_000.0), INSERT_ID);
    add(
        "write/transaction-log",
        point_lookup_with(|q| {
            q.text = "INSERT INTO [shop].[customers] ([name], [email]) VALUES (@P1, @P2)".into();
            q.rows = Some(750.0);
            q.costs = Some(culprit_core::QueryCosts {
                log_bytes: Some(3_000.0 * 900.0),
                log_share: Some(0.6),
                ..Default::default()
            });
        }),
        "7",
    );
    let mut s = heavy_insert(insert, Some(6_000.0));
    query_mut(&mut s, INSERT_ID).costs.as_mut().unwrap().log_share = Some(0.3);
    add("write/per-row-without-share", s, INSERT_ID);

    // Fixes for a statement's kind, its runs or its plan.
    add("fix/key-lookups", point_lookup(20_000.0, 40.0), "7");
    for (name, engine, id, costs) in [
        ("fix/big-sort-most", Engine::Postgres, "9065391607317686058", [90_000.0, 10_000.0, 0.0]),
        ("fix/big-sort-largest", Engine::Postgres, "9065391607317686058", [40_000.0, 35_000.0, 25_000.0]),
        ("fix/big-sort-costed-highest", Engine::Mysql, "19bd4ff468839ea3", [80_000.0, 20_000.0, 0.0]),
    ] {
        let mut s = if engine == Engine::Mysql { mysql_demo() } else { postgres_demo() };
        query_mut(&mut s, id).plan = Some(sort_plan(engine, costs));
        add(name, s, id);
    }
    add("fix/fewer-runs-finds-nothing", point_lookup_with(|q| q.rows = Some(0.0)), "7");
    let fast_read = |q: &mut QueryStat| {
        (q.plan, q.executions, q.mean_time_ms, q.total_time_ms) = (None, 30_000.0, 1.0, 30_000.0);
        q.rows = Some(30_000.0);
    };
    let mut s = postgres_demo();
    fast_read(query_mut(&mut s, INSERT_ID));
    retotal(&mut s);
    add("fix/fewer-runs-not-cheap", s, INSERT_ID);
    let slow = |text: &'static str| {
        move |q: &mut QueryStat| {
            q.text = text.into();
            (q.plan, q.mean_time_ms, q.total_time_ms) = (None, 50.0, 50.0 * q.executions);
        }
    };
    add(
        "fix/per-kind-write",
        pg_update_with(
            "UPDATE products SET name = $1 WHERE id = $2",
            slow("UPDATE products SET name = $1 WHERE id = $2"),
        ),
        UPDATE_ID,
    );
    add(
        "fix/per-kind-locking",
        pg_update_with(
            "SELECT id FROM orders WHERE id = $1 FOR UPDATE",
            slow("SELECT id FROM orders WHERE id = $1 FOR UPDATE"),
        ),
        UPDATE_ID,
    );
    let mut s = postgres_demo();
    query_mut(&mut s, "-3722457391793582113").plan = None;
    add("fix/per-kind-aggregate-kept-up-to-date", s, "-3722457391793582113");
    let mut s = postgres_demo();
    query_mut(&mut s, "2885972271926804069").plan = None;
    add("fix/per-kind-aggregate", s, "2885972271926804069");
    let mut s = postgres_demo();
    let q = query_mut(&mut s, INSERT_ID);
    q.text = APPS_QUERY.into();
    (q.executions, q.rows, q.total_time_ms, q.mean_time_ms) = (1000.0, Some(2000.0), 1.0, 0.001);
    q.plan = Some(apps_plan());
    add("fix/rewrite", s, INSERT_ID);
    for (engine, name) in [(Engine::Mssql, "fix/counter-windowed"), (Engine::Postgres, "fix/counter-running-totals")] {
        let mut s = mssql_demo();
        s.server.engine = engine;
        let w = s.workload.as_mut().unwrap();
        let mut q = w.queries[0].clone();
        q.text = "UPDATE shop.counters SET n = n + 1, updated_at = now() WHERE id = $1 RETURNING n".into();
        q.id = "compound-counter".into();
        (q.object_name, q.plan, q.trend, q.first_seen, q.last_seen) = (None, None, None, None, None);
        q.plans.clear();
        q.waits.clear();
        (q.executions, q.mean_time_ms, q.total_time_ms) = (100_000.0, 1.0, 100_000.0);
        (q.rows, q.logical_reads) = (Some(q.executions), Some(3.0 * q.executions));
        (w.windowed, w.window_hours, w.hourly_time_ms) = (engine == Engine::Mssql, 24.0, None);
        (w.total_time_ms, w.total_executions) = (q.total_time_ms, q.executions);
        w.queries = vec![q];
        add(name, s, "compound-counter");
    }
    add("fix/session-transaction", postgres_demo(), "6097083398544187049");
    let mut s = postgres_demo();
    query_mut(&mut s, "4854991825702068830").text = "DISCARD ALL".into();
    add("fix/session-reset", s, "4854991825702068830");
    let mut s = mssql_demo();
    query_mut(&mut s, "7").text = "SET TRANSACTION ISOLATION LEVEL READ COMMITTED".into();
    add("fix/session-windowed", s, "7");

    // Metrics.
    let mut s = point_lookup_with(|q| q.rows = Some(3.0));
    add("metric/rows-in-runs", s.clone(), "7");
    query_mut(&mut s, "7").cpu_time_ms = None;
    add("metric/no-cpu", s, "7");

    out
}

/// Fixes and verify steps worded on their own: for a scan's step, a pool's statement, what to measure, and each thing a
/// later scan shows on each kind of source.
fn other_scenarios() -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let fix = |f: Fix| format!("fix: {}", f.summary);
    out.push(("scan/or-across-tables", fix(scan_fix_on(&postgres_demo(), 2.0))));
    out.push(("scan/or-across-tables-less-than-a-row", fix(scan_fix_on(&postgres_demo(), 0.5))));
    out.push(("scan/or-across-tables-2.5-rows", fix(scan_fix_on(&postgres_demo(), 2.5))));
    out.push(("scan/or-across-tables-1-row", fix(scan_fix_on(&postgres_demo(), 1.02))));
    out.push(("scan/read-less", fix(scan_fix_on(&postgres_demo(), 5_000.0))));
    let mut snapshot = postgres_demo();
    let owner = index(&snapshot, "apps", "idx_apps_owner", &["owner_id"]);
    let key = index(&snapshot, "app_shares", "PK_app_shares", &["app_id", "email"]);
    snapshot.indexes.extend([owner, key]);
    out.push(("scan/or-across-tables-coverage", fix(scan_fix_on(&snapshot, 2.0))));
    let mut partial = index(&snapshot, "app_shares", "app_shares_email_idx", &["email"]);
    partial.filter = Some("(email IS NOT NULL)".into());
    snapshot.indexes.push(partial);
    out.push(("scan/or-across-tables-coverage-partial", fix(scan_fix_on(&snapshot, 2.0))));
    // A condition on one table above the scan, and none.
    let narrowed = |predicate: Option<&str>| {
        let s = postgres_demo();
        let mut plan = apps_plan();
        plan.root.children[0].predicate = predicate.map(str::to_string);
        let mut q = s.workload.as_ref().expect("workload").queries[0].clone();
        q.text = "SELECT a.id, a.name FROM apps a LEFT JOIN app_shares s ON s.app_id = a.id WHERE a.deleted_at IS NULL ORDER BY a.name".into();
        q.rows = Some(2.0 * q.executions);
        q.plan = Some(plan.clone());
        let scan = plan.nodes().into_iter().find(|n| n.id == 7).expect("the scan of apps");
        let (remedy, _) = heavy_remedy(&s, Some((&plan, scan)), None, &q, statement_kind(&q.text), false);
        remedy.fix(&s, &q)
    };
    out.push(("scan/narrowed-by-a-condition", fix(narrowed(Some("((a.owner_id)::text = $1)")))));
    out.push(("scan/narrowed-unseen", fix(narrowed(None))));
    // The cross-table OR on its own, without the rows a run returns.
    let s = postgres_demo();
    let mut q = s.queries()[0].clone();
    q.text = APPS_QUERY.into();
    (q.executions, q.rows, q.total_time_ms, q.mean_time_ms) = (1000.0, None, 1.0, 0.001);
    let mut large = apps_plan();
    large.root.children[0].children[1].children[0].table_rows = Some(6_285_000.0);
    large.root.children[0].children[1].children[0].rows_read = Some(6_285_000.0);
    large.root.children[0].children[1].children[0].est_rows = 5_880_000.0;
    let rewrite = late_filter_rewrite(&s, &q, &large).expect("a rewrite");
    out.push((
        "rewrite/unmeasured",
        format!(
            "title: {}\ndetail: {}\nfix: {}\nverify: {}",
            rewrite.title, rewrite.detail, rewrite.fix, rewrite.verify
        ),
    ));
    // What a cheap write's runs cost, with no rows counted: its log alone.
    let s = heavy_insert("INSERT INTO products (sku, name) VALUES ($1, $2)", None);
    let q = s.queries().iter().find(|q| q.id == INSERT_ID).expect("query");
    out.push(("write-cost/log-only", format!("summary: {}", write_cost(&s, q))));
    let mut s = heavy_insert("INSERT INTO products (sku, name) VALUES ($1, $2)", None);
    query_mut(&mut s, INSERT_ID).costs = None;
    let q = s.queries().iter().find(|q| q.id == INSERT_ID).expect("query");
    out.push(("write-cost/nothing-counted", format!("summary: {}", write_cost(&s, q))));
    // The pool's statements.
    for (name, engine, text) in [
        ("session/transaction", Engine::Postgres, "BEGIN"),
        ("session/ping", Engine::Mysql, "/* ping */ SELECT 1"),
        ("session/settings", Engine::Mysql, "SET NAMES utf8mb4"),
        ("session/engine", Engine::Mssql, "SET TRANSACTION ISOLATION LEVEL READ COMMITTED"),
        ("session/reset", Engine::Postgres, "DISCARD ALL"),
    ] {
        out.push((name, format!("fix: {}", session_fix(engine, text))));
    }
    // What to measure first, on an engine that keeps the last actual plan and on one that doesn't.
    for (name, engine) in [("measure/sql-server", Engine::Mssql), ("measure/postgres", Engine::Postgres)] {
        let mut s = lookups_for_nothing();
        s.server.engine = engine;
        let q = s.queries().iter().find(|q| q.id == "7").expect("query").clone();
        out.push((name, fix(Remedy::Measure { schema: "shop".into(), table: "customers".into() }.fix(&s, &q))));
    }
    // Each thing a later scan shows: windowed (Query Store, a slow log) and on running totals, in pages and in rows
    // examined.
    let sources = [
        ("verify/query-store", mssql_demo(), "7"),
        ("verify/slow-log", slow_log(mysql_demo()), "c7b5ac9bb4bcecfe"),
        ("verify/pg-stat-statements", postgres_demo(), INSERT_ID),
        ("verify/performance-schema", mysql_demo(), "c7b5ac9bb4bcecfe"),
    ];
    let expects = [
        ("measure", Expect::Measure),
        ("storage", Expect::Storage),
        ("recorded-wait", Expect::RecordedWait),
        ("cheaper-runs", Expect::CheaperRuns),
        ("fewer-runs", Expect::FewerRuns),
        ("counter", Expect::Counter),
        ("session", Expect::Session),
        ("average", Expect::Average),
        ("time-or-stops", Expect::TimeOrStops),
    ];
    for (name, s, id) in &sources {
        let q = s.queries().iter().find(|q| q.id == *id).expect("query");
        let lines: Vec<String> = expects
            .iter()
            .map(|(expect_name, expect)| format!("{expect_name}: {}", verify_line(s, q, *expect)))
            .collect();
        out.push((name, lines.join("\n")));
    }
    let mut s = postgres_demo();
    query_mut(&mut s, "6097083398544187049").text = "COMMIT".into();
    let q = s.queries().iter().find(|q| q.id == "6097083398544187049").expect("query");
    out.push(("verify/session-statement", verify_line(&s, q, Expect::Session)));
    let info = heavy_query_info();
    out.push(("info", format!("title: {}\ndescription: {}", info.title, info.description)));
    out
}

// Every shape this check words reads as it did, word for word.
#[test]
fn every_shape_reads_in_english_word_for_word() {
    let said: Vec<(&str, String)> = scenarios().into_iter().chain(other_scenarios()).collect();
    assert_eq!(
        said.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        PINNED.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "every scenario is pinned, in order"
    );
    let changed: Vec<String> = said
        .iter()
        .zip(PINNED)
        .filter(|((_, text), (_, pinned))| text != pinned)
        .map(|((name, text), (_, pinned))| format!("{name}\n  now:    {text}\n  pinned: {pinned}"))
        .collect();
    assert!(changed.is_empty(), "{} scenarios read differently:\n{}", changed.len(), changed.join("\n"));
}

/// What each scenario says, word for word: its English as it read before its words moved to locales/en.json.
const PINNED: &[(&str, &str)] = &[
    (
        "title/share",
        r#"title: A read from order_items takes 4% of database time
summary: It ran 397 times in 5 hours at 17 ms on average: 4% of all database time in the 5 hours since statistics were reset. 93% of its estimated cost is a seq scan of public.order_items that reads 1.0M rows.
impact: 4% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: The plan scans public.order_items and filters afterwards. An index on the filtered columns turns that into a seek; open the plan to see the predicate.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "title/share-stopped",
        r#"title: An insert into products took 6% of database time until it stopped
summary: It ran about 333 times a day at 5 ms on average: 6% of all database time in the 6 days since statistics were reset. Its plan had nothing to make faster: checking for rows still cost work when a run changed nothing. Each run inserted 0.25 rows on average (75% of its runs inserted nothing) and wrote 1.7 KB of WAL, 99% of all the WAL the statements wrote: 6.6 KB and 2.7 full-page images (a page logged whole the first time it changes after a checkpoint) for each row it inserted.
impact: 6% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows changed per run: 0.25
fix: Each run was fast, but most of its runs changed nothing: the cost was in sending them anyway. 75% of its runs inserted nothing: skip the statement where the application already knows the row exists (it read the row, or wrote it earlier, and keeps the value for as long as it holds), and send it only when the row may be missing. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "title/share-idle-stopped",
        r#"title: An update of products took 87% of database time until it stopped
summary: It ran about 8 times a day at 19.0 s on average: 87% of all database time in the 6 days since statistics were reset.
impact: 87% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows changed: 1 row in 50 runs
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "title/nothing-pages",
        r#"title: A read from customers reads 750 pages a run to return nothing
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: After the change ships, re-scan: this query's pages read and time per run should drop, and its share of database time with them."#,
    ),
    (
        "title/less-than-a-row-pages",
        r#"title: A read from customers reads 750 pages a run to return less than a row
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run: 0.3
metric: CPU share of its time
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: After the change ships, re-scan: this query's pages read and time per run should drop, and its share of database time with them."#,
    ),
    (
        "title/nothing-rows-examined",
        r#"title: A read from products examines 500 rows a run to return nothing
summary: It ran 30K times in 2 minutes at 0.04 ms on average: 2% of all database time in the 2 minutes since statistics were reset.
impact: 2% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "title/nothing-pages-stopped",
        r#"title: A read from customers read 500 pages a run to return nothing
summary: It ran about 1.4K times a day at 0.02 ms on average: 20% of all database time in the 6 days since statistics were reset.
impact: 20% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "title/nothing-rows-examined-stopped",
        r#"title: A read from products examined 500 rows a run to return nothing
summary: It ran about 15K times a day at 0.04 ms on average from Oct 6 to Oct 8: 2% of all database time in the 6 days since statistics were reset.
impact: 2% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "title/many-per-row-pages",
        r#"title: A read from orders reads 3.2K pages for every row it returns
summary: It ran 852 times in 5 hours at 44 ms on average: 24% of all database time in the 5 hours since statistics were reset. 98% of its estimated cost is a seq scan of public.orders that reads 2.0M rows.
impact: 24% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: The plan scans public.orders and filters afterwards. An index on the filtered columns turns that into a seek; open the plan to see the predicate.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "title/many-per-row-rows-examined",
        r#"title: A read from orders examines 100K rows for every row it returns
summary: It ran 60 times in 2 minutes at 71 ms on average: 9% of all database time in the 2 minutes since statistics were reset. Its only step with a cost is a table scan of shop.orders that examines 500K rows a run (MySQL puts no cost on the sort).
impact: 9% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "title/many-per-row-pages-stopped",
        r#"title: A read from orders read 3.2K pages for every row it returned
summary: It ran about 142 times a day at 44 ms on average: 24% of all database time in the 6 days since statistics were reset. 98% of its estimated cost is a seq scan of public.orders that reads 2.0M rows.
impact: 24% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: The plan scans public.orders and filters afterwards. An index on the filtered columns turns that into a seek; open the plan to see the predicate.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "idle/sleep",
        r#"title: A call to pg_sleep takes 13% of database time, sleeping
summary: It ran once in 5 hours at 20.0 s: 13% of all database time in the 5 hours since statistics were reset. It calls pg_sleep: the time is a deliberate pause, not work, and no index or rewrite makes it shorter.
impact: 13% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find who sends it (a health check, a test, a retry loop with a back-off) and whether the pause belongs in the database at all: while it sleeps it holds a connection, and any locks its transaction took.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time should drop, or it should stop running. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "idle/app-lock",
        r#"title: A call to pg_advisory_lock takes 13% of database time, waiting for an application lock
summary: It ran once in 5 hours at 20.0 s: 13% of all database time in the 5 hours since statistics were reset. It calls pg_advisory_lock, which waits until the session holding that lock releases it: its time is how long others hold the lock, not work it does.
impact: 13% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: It ran once in the 5 hours since statistics were reset, so there is no wait to catch now; if it runs again, have its lock waits logged (log_lock_waits) to see what holds them. Shorten the work done while the lock is held, or take the lock without waiting (a try-lock, or a lock timeout) and retry later.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time should drop, or it should stop running. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "unknown-cause/pg-write",
        r#"title: An update of products takes 12% of database time
summary: It ran once in 5 hours at 19.0 s: 12% of all database time in the 5 hours since statistics were reset.
impact: 12% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "unknown-cause/locking-clause",
        r#"title: A read from orders takes 45% of database time
summary: It ran 3 times in 5 hours at 38.4 s on average: 45% of all database time in the 5 hours since statistics were reset.
impact: 45% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find this statement in the application: it locks the rows it finds until its transaction ends. Check that it locks only the rows the transaction goes on to change, and that the transaction commits soon after.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "unknown-cause/locking-plan",
        r#"title: A read from orders takes 12% of database time
summary: It ran once in 5 hours at 19.0 s: 12% of all database time in the 5 hours since statistics were reset.
impact: 12% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find this statement in the application: it locks the rows it finds until its transaction ends. Check that it locks only the rows the transaction goes on to change, and that the transaction commits soon after.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "unknown-cause/mssql-write",
        r#"title: An update of support_tickets takes 99% of database time
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. 99% of its estimated cost is a key lookup of shop.support_tickets.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows per run
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: After the change ships, re-scan: this query's pages read and time per run should drop, and its share of database time with them."#,
    ),
    (
        "unknown-cause/mysql-write",
        r#"title: An update of orders takes 100% of database time
summary: It ran 30K times in 2 minutes at 2.0 s on average: 100% of all database time in the 2 minutes since statistics were reset.
impact: 100% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "idle/lock-time",
        r#"title: An update of orders takes 100% of database time, with at least 13 h 20 min of recorded waiting (Locks)
summary: It ran 30K times in 2 minutes at 2.0 s on average: 100% of all database time in the 2 minutes since statistics were reset. performance_schema recorded at least 13 h 20 min of waiting (Locks) for this statement.
impact: 100% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
fix: Find what holds the locks while it waits (Live shows the blocking chain). After identifying the holder and lock resource, inspect transaction duration and access paths; shorter transactions, an index or a rewrite may reduce contention.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there recorded wait totals and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "idle/qs-locks-most",
        r#"title: A read from support_tickets takes 99% of database time, with at least 54 min of recorded waiting (Lock)
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Query Store recorded at least 54 min of waiting (Lock); measured CPU time is 1 min.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Find what holds the locks while it waits (Live shows the blocking chain). After identifying the holder and lock resource, inspect transaction duration and access paths; shorter transactions, an index or a rewrite may reduce contention.
verify: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change."#,
    ),
    (
        "idle/qs-locks-share",
        r#"title: A read from support_tickets takes 99% of database time, with at least 18 min of recorded waiting (Lock)
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Query Store recorded at least 18 min of waiting (Lock); measured CPU time is 1 min.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Find what holds the locks while it waits (Live shows the blocking chain). After identifying the holder and lock resource, inspect transaction duration and access paths; shorter transactions, an index or a rewrite may reduce contention.
verify: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change."#,
    ),
    (
        "idle/qs-client-most",
        r#"title: A read from support_tickets takes 99% of database time, with at least 54 min of recorded waiting (Network IO)
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Query Store recorded at least 54 min of waiting (Network IO); measured CPU time is 1 min.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Identify the native network wait and participating request before changing result handling; if result consumption is slow, fetch fewer rows or separate reading results from application processing.
verify: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change."#,
    ),
    (
        "idle/qs-client-share",
        r#"title: A read from support_tickets takes 99% of database time, with at least 18 min of recorded waiting (Network IO)
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Query Store recorded at least 18 min of waiting (Network IO); measured CPU time is 1 min.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Identify the native network wait and participating request before changing result handling; if result consumption is slow, fetch fewer rows or separate reading results from application processing.
verify: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change."#,
    ),
    (
        "idle/qs-memory-most",
        r#"title: A read from support_tickets takes 99% of database time, with at least 54 min of recorded waiting (Memory)
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Query Store recorded at least 54 min of waiting (Memory); measured CPU time is 1 min.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Inspect the native memory wait and related queries in Health; use grant or allocation evidence to decide whether estimates, indexing, concurrency or memory configuration need attention.
verify: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change."#,
    ),
    (
        "idle/qs-memory-share",
        r#"title: A read from support_tickets takes 99% of database time, with at least 18 min of recorded waiting (Memory)
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Query Store recorded at least 18 min of waiting (Memory); measured CPU time is 1 min.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Inspect the native memory wait and related queries in Health; use grant or allocation evidence to decide whether estimates, indexing, concurrency or memory configuration need attention.
verify: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change."#,
    ),
    (
        "idle/qs-latch-most",
        r#"title: A read from support_tickets takes 99% of database time, with at least 54 min of recorded waiting (Buffer Latch)
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Query Store recorded at least 54 min of waiting (Buffer Latch); measured CPU time is 1 min.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Identify the native latch and resource while contention occurs, then compare that evidence with the hot-row and last-page findings before choosing an access-path or concurrency change.
verify: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change."#,
    ),
    (
        "idle/qs-latch-share",
        r#"title: A read from support_tickets takes 99% of database time, with at least 18 min of recorded waiting (Buffer Latch)
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Query Store recorded at least 18 min of waiting (Buffer Latch); measured CPU time is 1 min.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Identify the native latch and resource while contention occurs, then compare that evidence with the hot-row and last-page findings before choosing an access-path or concurrency change.
verify: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change."#,
    ),
    (
        "idle/little-cpu-pages",
        r#"title: A read from support_tickets takes 99% of database time, most of it waiting rather than working
summary: It ran 4.0K times in 5 hours at 900 ms on average: 99% of all database time in the last 5 hours. Only 2% of its time is CPU, and 5 pages per run take far less than 900 ms: most of each run is spent waiting.
impact: 99% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Find what it waits for while it runs: Live shows each session's wait and any blocking chain. Typical causes are locks other transactions hold, a slow client reading the results, or a call outside the database.
verify: Re-scan after the change: its average duration drops toward what its reads take."#,
    ),
    (
        "idle/little-cpu-rows-examined",
        r#"title: A read from products takes 100% of database time, most of it waiting rather than working
summary: It ran 30K times in 2 minutes at 2.0 s on average: 100% of all database time in the 2 minutes since statistics were reset. Only 2% of its time is CPU, and 10 rows examined per run take far less than 2.0 s: most of each run is spent waiting.
impact: 100% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
metric: CPU share of its time
fix: Find what it waits for while it runs: Live shows each session's wait and any blocking chain. Typical causes are locks other transactions hold, a slow client reading the results, or a call outside the database.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop toward what its reads take. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "unknown-cause/pg-over-totals",
        r#"title: An update of products takes 95% of database time
summary: It ran about 10 times a day at 1.9 s on average over 36 hours of Culprit's own scans: 95% of all database time in them (about 1 run a day and 19.0 s on average over the 40 days since statistics were reset).
impact: 95% of DB time
metric: Share of DB time over Culprit's scans
metric: Runs per day over Culprit's scans
metric: Runs per day over the 40 days since statistics were reset
metric: Average duration over Culprit's scans
metric: Average duration over the 40 days since statistics were reset
metric: Pages read per run over the 40 days since statistics were reset
metric: Rows changed over the 40 days since statistics were reset: 1 row in 40 runs
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "unknown-cause/mysql-over-totals",
        r#"title: An update of orders takes 95% of database time
summary: It ran 40 times at 1.9 s on average, on Oct 11 at 12:07 UTC: 95% of all database time in 36 hours of Culprit's own scans.
impact: 95% of DB time
metric: Share of DB time over Culprit's scans
metric: Runs over Culprit's scans
metric: Average duration over Culprit's scans
metric: Rows examined per run over the 40 days since statistics were reset
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "idle/lock-time-over-totals",
        r#"title: An update of orders takes 95% of database time, with at least 1 min of recorded waiting (Locks)
summary: It ran 40 times at 1.9 s on average, on Oct 11 at 12:07 UTC: 95% of all database time in 36 hours of Culprit's own scans. Over the 40 days since statistics were reset, performance_schema recorded at least 1 min of waiting (Locks) for this statement.
impact: 95% of DB time
metric: Share of DB time over Culprit's scans
metric: Runs over Culprit's scans
metric: Average duration over Culprit's scans
metric: Rows examined per run over the 40 days since statistics were reset
fix: It ran 40 times on Oct 11 at 12:07 UTC, too seldom to count on catching it while it waits. Have the next ones logged instead. Set slow_query_log to ON and long_query_time to 1; SET PERSIST keeps it across restarts. The slow query log then keeps each statement slower than 1 s with its lock time and when it ran; MySQL keeps no record of who held the lock, so match those times with what the application ran then. After identifying the holder and lock resource, inspect transaction duration and access paths; shorter transactions, an index or a rewrite may reduce contention.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there recorded wait totals and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "idle/little-cpu-over-totals",
        r#"title: A read from products takes 95% of database time, most of it waiting rather than working
summary: It ran 40 times at 1.9 s on average, on Oct 11 at 12:07 UTC: 95% of all database time in 36 hours of Culprit's own scans. Over the 40 days since statistics were reset, only 2% of its time is CPU, and 10 rows examined per run take far less than 2.0 s: most of each run is spent waiting.
impact: 95% of DB time
metric: Share of DB time over Culprit's scans
metric: Runs over Culprit's scans
metric: Average duration over Culprit's scans
metric: Rows examined per run over the 40 days since statistics were reset
metric: Rows returned per run over the 40 days since statistics were reset
metric: CPU share of its time
fix: Find what it waits for while it runs: Live shows each session's wait and any blocking chain. Typical causes are locks other transactions hold, a slow client reading the results, or a call outside the database.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop toward what its reads take. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "lead/step-share-examined",
        r#"title: A read from orders takes 9% of database time
summary: It ran 2.0K times in 2 minutes at 2.1 ms on average: 9% of all database time in the 2 minutes since statistics were reset. 100% of its estimated cost is an index range scan of shop.orders that examines 11K rows a run.
impact: 9% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it aggregates more rows than it needs, or whether the result could be kept up to date instead of recomputed on every run.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "lead/uncosted-temporary",
        r#"title: A read from orders takes 77% of database time
summary: It ran 20 times in 2 minutes at 1.7 s on average: 77% of all database time in the 2 minutes since statistics were reset. Its only step with a cost is a table scan of shop.orders that examines 500K rows a run (MySQL puts no cost on the sort and the temporary table).
impact: 77% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it aggregates more rows than it needs, or whether the result could be kept up to date instead of recomputed on every run.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "lead/uncosted-grouping-costliest",
        r#"title: A read from orders takes 77% of database time
summary: It ran 20 times in 2 minutes at 1.7 s on average: 77% of all database time in the 2 minutes since statistics were reset. Of the steps its plan puts a cost on, the costliest is a table scan of shop.orders that examines 500K rows a run (MySQL puts no cost on the sort and the grouping).
impact: 77% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it aggregates more rows than it needs, or whether the result could be kept up to date instead of recomputed on every run.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "lead/uncosted-spool",
        r#"title: A read from orders examines 100K rows for every row it returns
summary: It ran 60 times in 2 minutes at 71 ms on average: 9% of all database time in the 2 minutes since statistics were reset. Its only step with a cost is a table scan of shop.orders that examines 500K rows a run (MySQL puts no cost on the spool).
impact: 9% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "lead/step-costed-highest",
        r#"title: A read from orders reads 3.2K pages for every row it returns
summary: It ran 852 times in 5 hours at 44 ms on average: 24% of all database time in the 5 hours since statistics were reset. The step its plan costs highest is a seq scan of public.orders that reads 2.0M rows.
impact: 24% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "lead/lookups-looks-up",
        r#"title: A read from customers reads 750 pages a run to return nothing
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. The captured plan estimates 1 row from ix_customers_email (on email), then looks up each row it finds in the table to check segment. The statement's runs read 750 pages each and return nothing; those totals do not show how many rows each plan step handled.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Read a retained actual plan when the server has one; otherwise capture a representative actual plan in a test environment. Check how many rows each step reads and keeps, and collect IO measurements for the same run to locate the reads on shop.customers. Use that evidence to decide whether a change to statistics or the access path is justified.
verify: Use an actual plan to check row counts, and collect IO measurements for the same run to locate the reads. After a change supported by that evidence, compare equivalent runs: pages read and time per run should fall without changing the result."#,
    ),
    (
        "lead/lookups-rows-examined",
        r#"title: A read from customers examines 750 rows a run to return nothing
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. The captured plan estimates 1 row from ix_customers_email (on email), then looks up each row it finds in the table to check segment. The statement's runs examine 750 rows each and return nothing; those totals do not show how many rows each plan step handled.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
metric: CPU share of its time
fix: Capture an actual execution plan for a representative run in a test environment. Check how many rows each step reads and keeps, and collect IO measurements for the same run to locate the reads on shop.customers. Use that evidence to decide whether a change to statistics or the access path is justified.
verify: Use an actual plan to check row counts, and collect IO measurements for the same run to locate the reads. After a change supported by that evidence, compare equivalent runs: rows examined and time per run should fall without changing the result."#,
    ),
    (
        "lead/lookups-estimates-rows",
        r#"title: A read from customers reads 750 pages a run to return nothing
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. The captured plan estimates 3 rows from ix_customers_email (on email), then looks up each row it finds in the table to check segment. The statement's runs read 750 pages each and return nothing; those totals do not show how many rows each plan step handled.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Read a retained actual plan when the server has one; otherwise capture a representative actual plan in a test environment. Check how many rows each step reads and keeps, and collect IO measurements for the same run to locate the reads on shop.customers. Use that evidence to decide whether a change to statistics or the access path is justified.
verify: Use an actual plan to check row counts, and collect IO measurements for the same run to locate the reads. After a change supported by that evidence, compare equivalent runs: pages read and time per run should fall without changing the result."#,
    ),
    (
        "lead/lookups-checks-less-than-a-row",
        r#"title: A read from customers reads 750 pages a run to return less than a row
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. The captured plan estimates 1 row from ix_customers_email (on email) and checks each row it finds for segment. The statement's runs read 750 pages each and return less than a row each; those totals do not show how many rows each plan step handled.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run: 0.3
metric: CPU share of its time
fix: Read a retained actual plan when the server has one; otherwise capture a representative actual plan in a test environment. Check how many rows each step reads and keeps, and collect IO measurements for the same run to locate the reads on shop.customers. Use that evidence to decide whether a change to statistics or the access path is justified.
verify: Use an actual plan to check row counts, and collect IO measurements for the same run to locate the reads. After a change supported by that evidence, compare equivalent runs: pages read and time per run should fall without changing the result."#,
    ),
    (
        "lead/cheap-write",
        r#"title: An insert into customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run inserts 1 row on average.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows per run
metric: CPU share of its time
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. Insert several rows per statement where the application has them together (a multi-row VALUES, a table-valued parameter, or a bulk insert), in one transaction.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "lead/cheap-plan",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "lead/cheap-plan-storage-finds-nothing",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap: each run reads about 6 pages, and no step of it is where the time goes. Query Store recorded at least 2.6 s of waiting (Buffer IO) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned: 1 row in 3.0K runs
metric: CPU share of its time
fix: Each run reads about 6 pages in total; recorded I/O waiting is material relative to its execution time. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change. Most of its runs find nothing. If it checks whether a row exists before writing one (a duplicate or idempotency check), let the write do the check instead: with a unique index on those columns, an insert that handles the duplicate-key error (or an upsert) needs no read first. If it polls for work, ask less often when it finds none, or have the writer notify the poller.
verify: Compare I/O measurements and execution time for equivalent useful work, parameters and load before and after a justified change; retain it only when the measurements show an improvement without changing results."#,
    ),
    (
        "lead/cheap-plan-storage-rows-examined",
        r#"title: A read from products takes 2% of database time
summary: It ran 30K times in 2 minutes at 0.04 ms on average: 2% of all database time in the 2 minutes since statistics were reset. Its plan is cheap: each run examines about 1 row, and no step of it is where the time goes. performance_schema recorded at least 913 ms of waiting (wait/io/table/sql/handler) for this statement.
impact: 2% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Each run examines about 1 row in total; recorded I/O waiting is material relative to its execution time. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change. Fewer runs (a result cached where it's safe to reuse, calls batched) also save the reads.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there I/O measurements and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "lead/whole-scan-rewrite",
        r#"title: A read from apps takes 45% of database time
summary: It ran 49K times in 5 hours at 1.4 ms on average: 45% of all database time in the 5 hours since statistics were reset. Every run includes a seq scan of public.apps that reads 6.3K rows: a scan reads the whole table, and costs more as the table grows.
impact: 45% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 2.5 rows a run: they are narrowed later in the plan, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: idx_apps_owner on public.apps leads with owner_id; public.app_shares has no index that leads with email. Keep this structural risk under review as table size or call volume grows.
verify: Compare results, representative parameters, plans, reads and execution time for the original and rewritten query. Preserve permission checks and duplicate/NULL behavior. Evaluate index keys only from the rewritten branch predicates and measured selectivity."#,
    ),
    (
        "lead/unexplained-read",
        r#"title: A read from customers takes 21% of database time
summary: It ran 8.2K times in 5 hours at 5 ms on average: 21% of all database time in the 5 hours since statistics were reset.
impact: 21% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it reads more rows or columns than it needs.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "summary/recent-entry",
        r#"title: A read from customers takes 10% of database time
summary: Its entry in pg_stat_statements is recent: 6.0K of its 8.2K runs came during 36 hours of Culprit's own scans, where it ran about 4.0K times a day at 0.50 ms on average and took 10% of database time. Over the 40 days since statistics were reset it is <1%, a share diluted by all the time before its entry began counting. Its plan is cheap, so the share comes from how often it runs, not from any step of it.
impact: 10% of DB time
metric: Share of DB time over Culprit's scans
metric: Runs per day over Culprit's scans
metric: Average duration over Culprit's scans
metric: Pages read per run
metric: Rows returned per run
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "summary/scans-beside-totals",
        r#"title: A read from customers takes 10% of database time
summary: It ran about 200 times a day at 10 ms on average over 36 hours of Culprit's own scans: 10% of all database time in them (<1% of database time and 0.02 ms on average over the 40 days since statistics were reset). Its plan is cheap, so the share comes from how often it runs, not from any step of it.
impact: 10% of DB time
metric: Share of DB time over Culprit's scans
metric: Share of DB time over the 40 days since statistics were reset
metric: Runs per day over Culprit's scans
metric: Average duration over Culprit's scans
metric: Average duration over the 40 days since statistics were reset
metric: Pages read per run over the 40 days since statistics were reset
metric: Rows returned per run over the 40 days since statistics were reset
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "summary/ran-taking",
        r#"title: An update of products takes 13% of database time
summary: It ran 2 times in 5 hours, taking 20.0 s and 1.0 s: 13% of all database time in the 5 hours since statistics were reset.
impact: 13% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run: 0.5
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "summary/of-what-it-could-attribute",
        r#"title: A read from order_items takes 4% of database time
summary: It ran 397 times in 5 hours at 17 ms on average: 4% of the database time pg_stat_statements could attribute in the 5 hours since statistics were reset. 93% of its estimated cost is a seq scan of public.order_items that reads 1.0M rows.
impact: 4% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: The plan scans public.order_items and filters afterwards. An index on the filtered columns turns that into a seek; open the plan to see the predicate.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "summary/burst-counted",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times at 1 ms on average, on Oct 11, 07:44–07:47 UTC: 14% of all database time in the last 6 days. Its plan is cheap, so the share comes from how often it runs, not from any step of it.
impact: 14% of DB time
metric: Share of DB time
metric: Runs
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "summary/slow-log",
        r#"title: A read from orders takes 9% of logged-run time
summary: It ran 2.0K times in 2 minutes at 2.1 ms on average: 9% of all logged-run time in the last 2 minutes. 100% of its estimated cost is an index range scan of shop.orders that examines 11K rows a run.
impact: 9% of logged-run time
metric: Share of logged-run time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it aggregates more rows than it needs, or whether the result could be kept up to date instead of recomputed on every run.
verify: After the change ships, re-scan: this query's rows examined and time per run should drop, and its share of logged-run time with them."#,
    ),
    (
        "wait/storage",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap: each run reads about 6 pages, and no step of it is where the time goes. Query Store recorded at least 2.5 s of waiting (Buffer IO) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run reads about 6 pages in total; recorded I/O waiting is material relative to its execution time. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change. Fewer runs (a result cached where it's safe to reuse, calls batched) also save the reads.
verify: Compare I/O measurements and execution time for equivalent useful work, parameters and load before and after a justified change; retain it only when the measurements show an improvement without changing results."#,
    ),
    (
        "wait/storage-most",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap: each run reads about 6 pages, and no step of it is where the time goes. Query Store recorded at least 4.2 s of waiting (Buffer IO) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run reads about 6 pages in total; recorded I/O waiting is material relative to its execution time. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change. Fewer runs (a result cached where it's safe to reuse, calls batched) also save the reads.
verify: Compare I/O measurements and execution time for equivalent useful work, parameters and load before and after a justified change; retain it only when the measurements show an improvement without changing results."#,
    ),
    (
        "wait/locks",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it. Query Store recorded at least 2.5 s of waiting (Lock) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "wait/latches",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it. Query Store recorded at least 2.5 s of waiting (Buffer Latch) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "wait/log",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it. Query Store recorded at least 2.5 s of waiting (Tran Log IO) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "wait/client",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it. Query Store recorded at least 2.5 s of waiting (Network IO) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "wait/memory",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it. Query Store recorded at least 2.5 s of waiting (Memory) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "wait/compilation",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it. Query Store recorded at least 2.5 s of waiting (Compilation) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "wait/storage-files-not-cheap",
        r#"title: A read from customers takes <1% of database time
summary: It ran 8.2K times in 5 hours at 0.02 ms on average: <1% of all database time in the 5 hours since statistics were reset. pg_stat_statements recorded at least 116 ms of waiting (DataFileRead) for this statement.
impact: <1% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Each run reads about 4 pages in total; recorded I/O waiting is material relative to its execution time. Check whether it reads unnecessary rows or columns. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there I/O measurements and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "wait/storage-files-no-reads",
        r#"title: A read from customers takes <1% of database time
summary: It ran 8.2K times in 5 hours at 0.02 ms on average: <1% of all database time in the 5 hours since statistics were reset. pg_stat_statements recorded at least 116 ms of waiting (DataFileRead) for this statement.
impact: <1% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows returned per run
fix: Recorded I/O waiting is material relative to the statement’s execution time. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there I/O measurements and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "wait/storage-over-totals",
        r#"title: A read from customers takes 10% of database time
summary: It ran about 200 times a day at 10 ms on average over 36 hours of Culprit's own scans: 10% of all database time in them (<1% of database time and 0.02 ms on average over the 40 days since statistics were reset). Over the 40 days since statistics were reset, pg_stat_statements recorded at least 116 ms of waiting (DataFileRead) for this statement.
impact: 10% of DB time
metric: Share of DB time over Culprit's scans
metric: Share of DB time over the 40 days since statistics were reset
metric: Runs per day over Culprit's scans
metric: Average duration over Culprit's scans
metric: Average duration over the 40 days since statistics were reset
metric: Pages read per run over the 40 days since statistics were reset
metric: Rows returned per run over the 40 days since statistics were reset
fix: Each run reads about 4 pages in total; recorded I/O waiting is material relative to its execution time. Check whether it reads unnecessary rows or columns. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there I/O measurements and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "wait/storage-cheap-fewer-runs",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap: each run reads about 6 pages, and no step of it is where the time goes. Query Store recorded at least 2.6 s of waiting (Buffer IO) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run reads about 6 pages in total; recorded I/O waiting is material relative to its execution time. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change. Fewer runs (a result cached where it's safe to reuse, calls batched) also save the reads.
verify: Compare I/O measurements and execution time for equivalent useful work, parameters and load before and after a justified change; retain it only when the measurements show an improvement without changing results."#,
    ),
    (
        "wait/storage-finds-nothing-unique-filtered",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap: each run reads about 6 pages, and no step of it is where the time goes. Query Store recorded at least 2.6 s of waiting (Buffer IO) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned: 1 row in 3.0K runs
metric: CPU share of its time
fix: Each run reads about 6 pages in total; recorded I/O waiting is material relative to its execution time. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change. Most of its runs find nothing. If it checks whether a row exists before writing one (a duplicate or idempotency check), let the write do the check instead: this plan seeks ix_customers_email on shop.customers (unique for the rows where `[email] IS NOT NULL`), a unique index that exists already, so an insert that handles its duplicate-key error (or an upsert) needs no read first. If it polls for work, ask less often when it finds none, or have the writer notify the poller.
verify: Compare I/O measurements and execution time for equivalent useful work, parameters and load before and after a justified change; retain it only when the measurements show an improvement without changing results."#,
    ),
    (
        "wait/storage-finds-nothing-unique",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap: each run reads about 6 pages, and no step of it is where the time goes. Query Store recorded at least 2.6 s of waiting (Buffer IO) for this statement.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned: 1 row in 3.0K runs
metric: CPU share of its time
fix: Each run reads about 6 pages in total; recorded I/O waiting is material relative to its execution time. Inspect the native I/O wait and compare physical reads and storage latency under equivalent work and load before choosing a change. The recorded wait total alone does not establish a cache miss, eviction or insufficient memory. Check independent cache-pressure evidence before considering a memory or service-tier change. Most of its runs find nothing. If it checks whether a row exists before writing one (a duplicate or idempotency check), let the write do the check instead: this plan seeks ix_customers_email on shop.customers, a unique index that exists already, so an insert that handles its duplicate-key error (or an upsert) needs no read first. If it polls for work, ask less often when it finds none, or have the writer notify the poller.
verify: Compare I/O measurements and execution time for equivalent useful work, parameters and load before and after a justified change; retain it only when the measurements show an improvement without changing results."#,
    ),
    (
        "triggers/one-update",
        r#"title: An update of products takes 12% of database time
summary: It ran once in 5 hours at 19.0 s: 12% of all database time in the 5 hours since statistics were reset. products also fires a trigger on update (trg_audit_row), whose work runs inside this statement's time.
impact: 12% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
metric: Triggers on table: trg_audit_row
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "triggers/many-update",
        r#"title: An update of products takes 12% of database time
summary: It ran once in 5 hours at 19.0 s: 12% of all database time in the 5 hours since statistics were reset. products also fires triggers on update (trg_a, trg_b), whose work runs inside this statement's time.
impact: 12% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
metric: Triggers on table: trg_a, trg_b
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "triggers/insert",
        r#"title: An insert into products takes 12% of database time
summary: It ran once in 5 hours at 19.0 s: 12% of all database time in the 5 hours since statistics were reset. products also fires a trigger on insert (trg_audit_row), whose work runs inside this statement's time.
impact: 12% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
metric: Triggers on table: trg_audit_row
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "triggers/delete",
        r#"title: A delete from products takes 12% of database time
summary: It ran once in 5 hours at 19.0 s: 12% of all database time in the 5 hours since statistics were reset. products also fires a trigger on delete (trg_audit_row), whose work runs inside this statement's time.
impact: 12% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
metric: Triggers on table: trg_audit_row
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "triggers/merge",
        r#"title: A merge into products takes 12% of database time
summary: It ran once in 5 hours at 19.0 s: 12% of all database time in the 5 hours since statistics were reset. products also fires a trigger on write (trg_audit_row), whose work runs inside this statement's time.
impact: 12% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
metric: Triggers on table: trg_audit_row
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "summary/statistics-maintenance",
        r#"title: A statistics refresh on orders takes 100% of database time
summary: It ran 2 times in 5 hours at 10.0 s on average: 100% of all database time in the last 5 hours. This is optimizer statistics maintenance, not an update of application rows.
impact: 100% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: CPU share of its time
fix: Check whether this was an automatic refresh or scheduled maintenance. Keep necessary statistics current; review an unnecessarily frequent schedule or full scan only with evidence of its cost.
verify: Compare the frequency and duration of subsequent statistics refreshes under comparable load, together with query performance. A completed maintenance operation can remain in Query Store's history after any scheduling change."#,
    ),
    (
        "write/skip-insert",
        r#"title: An insert into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: checking for rows still costs work when a run changes nothing. Each run inserts 0.25 rows on average (75% of its runs insert nothing) and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote: 6.6 KB and 2.7 full-page images (a page logged whole the first time it changes after a checkpoint) for each row it inserts.
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run: 0.25
fix: Each run is fast, but most of its runs change nothing: the cost is in sending them anyway. 75% of its runs insert nothing: skip the statement where the application already knows the row exists (it read the row, or wrote it earlier, and keeps the value for as long as it holds), and send it only when the row may be missing. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/get-or-create",
        r#"title: An insert into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run inserts or updates 1 row on average (a run that finds the row there rewrites it: its DO UPDATE sets a column to the value it has, which still writes a new version of the row) and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint).
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. It is a get-or-create: its DO UPDATE sets a column to the value it has only so that RETURNING gives back the row already there, and that still writes a new version of the row (and its WAL) on every run that finds one. Skip the call where the application already has the row (it read or created it earlier and keeps it), or evaluate ON CONFLICT DO NOTHING RETURNING followed by a separate SELECT when no row is returned. That is not an equivalent atomic replacement: account for the transaction isolation level, concurrent deletion, a missing row and serialization failures with appropriate locking or retries. Preserve any required UPDATE-trigger effects; keep the atomic upsert if the application depends on them. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/get-or-create-changing-nothing",
        r#"title: An insert into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run inserts or updates 0.25 rows on average (a run that finds the row there rewrites it: its DO UPDATE sets a column to the value it has, which still writes a new version of the row) (75% of its runs insert or update nothing) and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote: 6.6 KB and 2.7 full-page images (a page logged whole the first time it changes after a checkpoint) for each row it inserts or updates.
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run: 0.25
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. It is a get-or-create: its DO UPDATE sets a column to the value it has only so that RETURNING gives back the row already there, and that still writes a new version of the row (and its WAL) on every run that finds one. Skip the call where the application already has the row (it read or created it earlier and keeps it), or evaluate ON CONFLICT DO NOTHING RETURNING followed by a separate SELECT when no row is returned. That is not an equivalent atomic replacement: account for the transaction isolation level, concurrent deletion, a missing row and serialization failures with appropriate locking or retries. Preserve any required UPDATE-trigger effects; keep the atomic upsert if the application depends on them. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/upsert",
        r#"title: An insert into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run inserts or updates 1 row on average and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint).
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. On a conflict it updates the row it finds, writing a new version of it even when the values are the same: give its DO UPDATE a WHERE that updates only a row whose values differ (`WHERE t.c IS DISTINCT FROM EXCLUDED.c`), and skip the call where the application knows nothing changed. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/batch-insert-unused-index",
        r#"title: An insert into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run inserts 1 row on average and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint).
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. Insert several rows per statement where the application has them together (a multi-row VALUES, arrays unnested into rows, or COPY). products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/batch-insert-unused-indexes",
        r#"title: An insert into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run inserts 1 row on average and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint).
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. Insert several rows per statement where the application has them together (a multi-row VALUES, arrays unnested into rows, or COPY). products_sku_idx and products_name_idx have had no reads in the 40 days since its statistics were last reset, yet every write keeps them up to date: dropping them (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/skip-update",
        r#"title: An update of products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: checking for rows still costs work when a run changes nothing. Each run updates 0.25 rows on average (at least 75% of its runs update nothing) and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote: 6.6 KB and 2.7 full-page images (a page logged whole the first time it changes after a checkpoint) for each row it updates.
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run: 0.25
fix: Each run is fast, but most of its runs change nothing: the cost is in sending them anyway. At least 75% of its runs update nothing: skip the statement where the application already knows there is nothing to update (the rows aren't there, or already hold the values). products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/skip-delete-stopped",
        r#"title: A delete from products took 6% of database time until it stopped
summary: It ran about 333 times a day at 5 ms on average: 6% of all database time in the 6 days since statistics were reset. Its plan had nothing to make faster: checking for rows still cost work when a run changed nothing. Each run deleted 0.25 rows on average (at least 75% of its runs deleted nothing) and wrote 1.7 KB of WAL, 99% of all the WAL the statements wrote: 6.6 KB and 2.7 full-page images (a page logged whole the first time it changes after a checkpoint) for each row it deleted.
impact: 6% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows changed per run: 0.25
fix: Each run was fast, but most of its runs changed nothing: the cost was in sending them anyway. At least 75% of its runs deleted nothing: skip the statement where the application already knows there is nothing to delete (the rows aren't there, or already hold the values). products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/batch-update",
        r#"title: An update of products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run updates 1 row on average and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint).
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. Update several rows per statement where the application has them together (an UPDATE over an array of keys (`= ANY($1)`), or joined to a VALUES list), in one transaction. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/batch-delete",
        r#"title: A delete from products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run deletes 1 row on average and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint).
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. Delete several rows per statement where the application has them together (an DELETE over an array of keys (`= ANY($1)`), or joined to a VALUES list), in one transaction. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/batch-merge",
        r#"title: A merge into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run changes 1 row on average and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint).
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. Change several rows per statement where the application has them together (a multi-row VALUES, arrays unnested into rows, or COPY), in one transaction. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/log-only",
        r#"title: An insert into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset.
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/cheap-stopped",
        r#"title: An insert into products took 6% of database time until it stopped
summary: It ran about 333 times a day at 5 ms on average: 6% of all database time in the 6 days since statistics were reset. Its plan had nothing to make faster: what a run cost was the write itself (its log, its index entries, the checks of its keys). Each run inserted 1 row on average and wrote 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint).
impact: 6% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run was fast and its plan had nothing to make faster: what cost was the write itself, every time it ran. Insert several rows per statement where the application has them together (a multi-row VALUES, arrays unnested into rows, or COPY). products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/not-cheap",
        r#"title: An insert into products takes 16% of database time
summary: It ran 30K times in 5 hours at 1 ms on average: 16% of all database time in the 5 hours since statistics were reset.
impact: 16% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast; what costs is the write itself, every time it runs. Insert several rows per statement where the application has them together (a multi-row VALUES, arrays unnested into rows, or COPY).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/not-cheap-stopped",
        r#"title: An insert into products took 86% of database time until it stopped
summary: It ran about 167K times a day at 1 ms on average: 86% of all database time in the 6 days since statistics were reset.
impact: 86% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run was fast; what cost was the write itself, every time it ran. Insert several rows per statement where the application has them together (a multi-row VALUES, arrays unnested into rows, or COPY).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/over-totals",
        r#"title: An insert into products takes 10% of database time
summary: It ran about 200 times a day at 10 ms on average over 36 hours of Culprit's own scans: 10% of all database time in them (about 50 runs a day and 5 ms on average over the 40 days since statistics were reset). Its plan has nothing to make faster: checking for rows still costs work when a run changes nothing. Each run inserts 0.25 rows on average over the 40 days since statistics were reset (75% of its runs insert nothing) and writes 1.7 KB of WAL, 99% of all the WAL the statements wrote: 6.6 KB and 2.7 full-page images (a page logged whole the first time it changes after a checkpoint) for each row it inserts.
impact: 10% of DB time
metric: Share of DB time over Culprit's scans
metric: Runs per day over Culprit's scans
metric: Runs per day over the 40 days since statistics were reset
metric: Average duration over Culprit's scans
metric: Average duration over the 40 days since statistics were reset
metric: Pages read per run over the 40 days since statistics were reset
metric: Rows changed per run over the 40 days since statistics were reset: 0.25
fix: Each run is fast, but most of its runs change nothing: the cost is in sending them anyway. 75% of its runs insert nothing over the 40 days since statistics were reset: skip the statement where the application already knows the row exists (it read the row, or wrote it earlier, and keeps the value for as long as it holds), and send it only when the row may be missing. products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "write/transaction-log",
        r#"title: An insert into customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan has nothing to make faster: checking for rows still costs work when a run changes nothing. Each run inserts 0.25 rows on average (75% of its runs insert nothing) and writes 900 B of transaction log, 60% of all the transaction log the statements wrote: 3.5 KB for each row it inserts.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows per run: 0.25
metric: CPU share of its time
fix: Each run is fast, but most of its runs change nothing: the cost is in sending them anyway. 75% of its runs insert nothing: skip the statement where the application already knows the row exists (it read the row, or wrote it earlier, and keeps the value for as long as it holds), and send it only when the row may be missing.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "write/per-row-without-share",
        r#"title: An insert into products takes 6% of database time
summary: It ran 2.0K times in 5 hours at 5 ms on average: 6% of all database time in the 5 hours since statistics were reset. Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run inserts 3 rows on average and writes 1.7 KB of WAL: 567 B and 0.2 full-page images (a page logged whole the first time it changes after a checkpoint) for each row it inserts.
impact: 6% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Each run is fast and its plan has nothing to make faster: what costs is the write itself, every time it runs. Insert several rows per statement where the application has them together (a multi-row VALUES, arrays unnested into rows, or COPY). products_sku_idx has had no reads in the 40 days since its statistics were last reset, yet every write keeps it up to date: dropping it (see unused-index) saves that work.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/key-lookups",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. 60% of its estimated cost is a key lookup of shop.customers.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Select fewer columns where possible. Test adding only the narrow columns needed by frequent reads as included columns, balancing saved lookups against index size and write cost.
verify: After the change ships, re-scan: this query's pages read and time per run should drop, and its share of database time with them."#,
    ),
    (
        "fix/big-sort-most",
        r#"title: A read from orders takes 45% of database time
summary: It ran 214 times in 5 hours at 332 ms on average: 45% of all database time in the 5 hours since statistics were reset. 90% of its estimated cost is a sort.
impact: 45% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Most of the work is sorting. An index whose key order matches the sort lets the database read rows already sorted.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/big-sort-largest",
        r#"title: A read from orders takes 45% of database time
summary: It ran 214 times in 5 hours at 332 ms on average: 45% of all database time in the 5 hours since statistics were reset. 40% of its estimated cost is a sort.
impact: 45% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: The largest part of the work is sorting. An index whose key order matches the sort lets the database read rows already sorted.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/big-sort-costed-highest",
        r#"title: A read from orders examines 100K rows for every row it returns
summary: It ran 60 times in 2 minutes at 71 ms on average: 9% of all database time in the 2 minutes since statistics were reset. Of the steps its plan puts a cost on, the costliest is a sort (MySQL puts no cost on the grouping).
impact: 9% of DB time
metric: Share of DB time
metric: Average duration
metric: Rows examined per run
metric: Rows returned per run
fix: The step the plan costs highest is sorting. An index whose key order matches the sort lets the database read rows already sorted.
verify: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/fewer-runs-finds-nothing",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Most of its runs find nothing. If it checks whether a row exists before writing one (a duplicate or idempotency check), let the write do the check instead: with a unique index on those columns, an insert that handles the duplicate-key error (or an upsert) needs no read first. If it polls for work, ask less often when it finds none, or have the writer notify the poller.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "fix/fewer-runs-not-cheap",
        r#"title: A read from customers takes 16% of database time
summary: It ran 30K times in 5 hours at 1 ms on average: 16% of all database time in the 5 hours since statistics were reset.
impact: 16% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Each run is fast; the cost is in how often it runs. Check whether the application needs every run: cache the result, batch the calls, or skip the statement when its answer is already known.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/per-kind-write",
        r#"title: An update of products takes <1% of database time
summary: It ran once in 5 hours at 50 ms: <1% of all database time in the 5 hours since statistics were reset.
impact: <1% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
fix: Find this statement in the application and check that it finds the rows it changes through an index, and changes no more rows per run than it needs (split large updates into batches).
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/per-kind-locking",
        r#"title: A read from orders takes <1% of database time
summary: It ran once in 5 hours at 50 ms: <1% of all database time in the 5 hours since statistics were reset.
impact: <1% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find this statement in the application: it locks the rows it finds until its transaction ends. Check that it locks only the rows the transaction goes on to change, and that the transaction commits soon after.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/per-kind-aggregate-kept-up-to-date",
        r#"title: A read from order_items takes 4% of database time
summary: It ran 397 times in 5 hours at 17 ms on average: 4% of all database time in the 5 hours since statistics were reset.
impact: 4% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it aggregates more rows than it needs, or whether the result could be kept up to date instead of recomputed on every run.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/per-kind-aggregate",
        r#"title: A read from events takes <1% of database time
summary: It ran 1.7K times in 5 hours at 0.07 ms on average: <1% of all database time in the 5 hours since statistics were reset.
impact: <1% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: Find this statement in the application and check whether it aggregates more rows than it needs.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/rewrite",
        r#"title: A read from apps takes <1% of database time
summary: It ran 1.0K times in 5 hours at 1 µs on average: <1% of all database time in the 5 hours since statistics were reset. 65% of its estimated cost is a seq scan of public.apps that reads 6.3K rows.
impact: <1% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 2 rows a run: they are narrowed later in the plan, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: public.apps has no index that leads with owner_id; public.app_shares has no index that leads with email. Keep this structural risk under review as table size or call volume grows.
verify: Compare results, representative parameters, plans, reads and execution time for the original and rewritten query. Preserve permission checks and duplicate/NULL behavior. Evaluate index keys only from the rewritten branch predicates and measured selectivity."#,
    ),
    (
        "fix/counter-windowed",
        r#"title: An update of counters takes 100% of database time
summary: It ran about 100K times a day at 1 ms on average: 100% of all database time in the last 24 hours.
impact: 100% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows per run
metric: CPU share of its time
fix: The SQL does not establish an isolated counter-only operation: preserve every assignment, returned value, row filter and transaction guarantee. Check the caller before changing it. If it already has independent work that can safely share a call, consider batching that work; per-parent numbering and metadata updates may require this atomic UPDATE. A sequence is not a replacement for the whole statement.
verify: Compare time and any avoidable calls under equivalent workload, checking that numbering, metadata updates, returned values and transaction guarantees remain correct. Required atomic calls need not decrease."#,
    ),
    (
        "fix/counter-running-totals",
        r#"title: An update of counters takes 100% of database time
summary: It ran about 100K times a day at 1 ms on average: 100% of all database time in the 24 hours since statistics were reset.
impact: 100% of DB time
metric: Share of DB time
metric: Runs per day
metric: Average duration
metric: Pages read per run
metric: Rows changed per run
metric: CPU share of its time
fix: The SQL does not establish an isolated counter-only operation: preserve every assignment, returned value, row filter and transaction guarantee. Check the caller before changing it. If it already has independent work that can safely share a call, consider batching that work; per-parent numbering and metadata updates may require this atomic UPDATE. A sequence is not a replacement for the whole statement.
verify: Query Store keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare time and avoidable calls for equivalent work, preserving numbering, metadata updates, returned values and transaction guarantees. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "fix/session-transaction",
        r#"title: COMMIT takes <1% of database time
summary: It ran once in 5 hours at 2 µs: <1% of all database time in the 5 hours since statistics were reset.
impact: <1% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: The driver or the connection pool sends this around the application's statements, not the application's own code: check whether it wraps every statement in a transaction of its own (each commit waits for the log to reach disk), and run the statements that belong together in one transaction.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare avoidable session-call overhead for equivalent useful work, preserving transaction guarantees and session isolation; required calls need not decrease."#,
    ),
    (
        "fix/session-reset",
        r#"title: DISCARD ALL takes <1% of database time
summary: It ran once in 5 hours at 2 µs: <1% of all database time in the 5 hours since statistics were reset.
impact: <1% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
fix: This resets session state and is commonly sent by a driver or connection pool; the statement statistics do not identify its caller or pooling mode. Inspect the application's connection lifecycle and the driver or pool configuration, and benchmark any change against the measured absolute cost shown here. Keep resets that prevent session state leaking between users. Npgsql arranges its reset on close and sends it on reuse; PgBouncer normally resets on release in session pooling, and normally does not in transaction pooling. Consider changing reset behavior only after verifying that every caller safely handles retained session state.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare avoidable session-call overhead for equivalent useful work, preserving transaction guarantees and session isolation; required calls need not decrease."#,
    ),
    (
        "fix/session-windowed",
        r#"title: A query takes 54% of database time
summary: It ran 1.2K times in 5 hours at 18 ms on average: 54% of all database time in the last 5 hours. 88% of its estimated cost is an index scan of shop.customers that reads 200K rows.
impact: 54% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned per run
metric: CPU share of its time
fix: The driver or the connection pool sends this around the application's statements, not the application's own code: a pooled connection returns to the login's defaults each time it is reused (sp_reset_connection), so options set per session are sent again on every checkout. Set them once instead: as the database's or the login's defaults, or in the connection string where the driver allows.
verify: Compare avoidable session-call overhead for equivalent useful work. Verify transaction guarantees and session isolation; required calls need not decrease."#,
    ),
    (
        "metric/rows-in-runs",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned: 3 rows in 3.0K runs
metric: CPU share of its time
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Most of its runs find nothing. If it checks whether a row exists before writing one (a duplicate or idempotency check), let the write do the check instead: with a unique index on those columns, an insert that handles the duplicate-key error (or an upsert) needs no read first. If it polls for work, ask less often when it finds none, or have the writer notify the poller.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "metric/no-cpu",
        r#"title: A read from customers takes 14% of database time
summary: It ran 3.0K times in 5 hours at 1 ms on average: 14% of all database time in the last 5 hours. Its plan is cheap, so the share comes from how often it runs, not from any step of it.
impact: 14% of DB time
metric: Share of DB time
metric: Average duration
metric: Pages read per run
metric: Rows returned: 3 rows in 3.0K runs
fix: Each run is fast and its plan is already cheap; the cost is in how often it runs. Most of its runs find nothing. If it checks whether a row exists before writing one (a duplicate or idempotency check), let the write do the check instead: with a unique index on those columns, an insert that handles the duplicate-key error (or an upsert) needs no read first. If it polls for work, ask less often when it finds none, or have the writer notify the poller.
verify: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise."#,
    ),
    (
        "scan/or-across-tables",
        r#"fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 2 rows a run: they are narrowed later in the plan, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: public.apps has no index that leads with owner_id; public.app_shares has no index that leads with email. Keep this structural risk under review as table size or call volume grows."#,
    ),
    (
        "scan/or-across-tables-less-than-a-row",
        r#"fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns less than a row a run: they are narrowed later in the plan, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: public.apps has no index that leads with owner_id; public.app_shares has no index that leads with email. Keep this structural risk under review as table size or call volume grows."#,
    ),
    (
        "scan/or-across-tables-2.5-rows",
        r#"fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 2.5 rows a run: they are narrowed later in the plan, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: public.apps has no index that leads with owner_id; public.app_shares has no index that leads with email. Keep this structural risk under review as table size or call volume grows."#,
    ),
    (
        "scan/or-across-tables-1-row",
        r#"fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 1 row a run: they are narrowed later in the plan, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: public.apps has no index that leads with owner_id; public.app_shares has no index that leads with email. Keep this structural risk under review as table size or call volume grows."#,
    ),
    (
        "scan/read-less",
        r#"fix: The plan reads all of public.apps and keeps about 94% of the rows, too many for an index to help. Read only the rows and columns the caller needs, or page through them."#,
    ),
    (
        "scan/or-across-tables-coverage",
        r#"fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 2 rows a run: they are narrowed later in the plan, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: idx_apps_owner on public.apps leads with owner_id; public.app_shares has no index that leads with email. Keep this structural risk under review as table size or call volume grows."#,
    ),
    (
        "scan/or-across-tables-coverage-partial",
        r#"fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 2 rows a run: they are narrowed later in the plan, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: idx_apps_owner on public.apps leads with owner_id; app_shares_email_idx on public.app_shares leads with email (partial index: verify the branch implies (email IS NOT NULL)). Keep this structural risk under review as table size or call volume grows."#,
    ),
    (
        "scan/narrowed-by-a-condition",
        r#"fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 2 rows a run: they are narrowed later in the plan, by `((a.owner_id)::text = $1)`. Where that condition can be applied to public.apps itself, an index on the columns it compares lets the plan read only the rows needed."#,
    ),
    (
        "scan/narrowed-unseen",
        r#"fix: The plan reads all of public.apps and keeps about 94% of its rows, but the statement returns about 2 rows a run: they are narrowed later in the plan (by a join or a limit). Open the plan to see where; a condition on public.apps itself, with an index on the columns it compares, lets the plan read only the rows needed."#,
    ),
    (
        "rewrite/unmeasured",
        r#"title: A filter across tables can require a whole-table read
detail: The captured plan scans public.apps before an OR across tables narrows its rows. This structure can become expensive as the tables grow; the plan does not measure the benefit or semantics of a rewrite.
fix: The captured plan reads public.apps before a filter across tables is applied, by `(((a.owner_id)::text = $1) OR ((s.email)::text = $2))`. This OR spans different tables, so a single index on the scan's filter does not address it. Evaluate a UNION rewrite or an equivalent EXISTS form that lets each branch use its own access path. Preserve the original joins, permission checks, duplicate handling, NULL behavior, grouping and ordering. Compare representative rewritten plans and reads before deciding whether a branch needs another index. Existing coverage: public.apps has no index that leads with owner_id; public.app_shares has no index that leads with email. Keep this structural risk under review as table size or call volume grows.
verify: Compare results, representative parameters, plans, reads and execution time for the original and rewritten query. Preserve permission checks and duplicate/NULL behavior. Evaluate index keys only from the rewritten branch predicates and measured selectivity."#,
    ),
    (
        "write-cost/log-only",
        r#"summary: Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys). Each run writes 1.7 KB of WAL, 99% of all the WAL the statements wrote, with 0.7 full-page images (a page logged whole the first time it changes after a checkpoint)."#,
    ),
    (
        "write-cost/nothing-counted",
        r#"summary: Its plan has nothing to make faster: what a run costs is the write itself (its log, its index entries, the checks of its keys)."#,
    ),
    (
        "session/transaction",
        r#"fix: The driver or the connection pool sends this around the application's statements, not the application's own code: check whether it wraps every statement in a transaction of its own (each commit waits for the log to reach disk), and run the statements that belong together in one transaction."#,
    ),
    (
        "session/ping",
        r#"fix: The driver or the connection pool sends this around the application's statements, not the application's own code: it checks that a connection is still alive. Check how often the pool does that (on every checkout, or on a timer) and check less often."#,
    ),
    (
        "session/settings",
        r#"fix: The driver or the connection pool sends this around the application's statements, not the application's own code: look at what the pool runs each time it hands out a connection (session settings, a reset), and set what the application needs once instead: in the connection string, or as the role's or the server's defaults."#,
    ),
    (
        "session/engine",
        r#"fix: The driver or the connection pool sends this around the application's statements, not the application's own code: a pooled connection returns to the login's defaults each time it is reused (sp_reset_connection), so options set per session are sent again on every checkout. Set them once instead: as the database's or the login's defaults, or in the connection string where the driver allows."#,
    ),
    (
        "session/reset",
        r#"fix: This resets session state and is commonly sent by a driver or connection pool; the statement statistics do not identify its caller or pooling mode. Inspect the application's connection lifecycle and the driver or pool configuration, and benchmark any change against the measured absolute cost shown here. Keep resets that prevent session state leaking between users. Npgsql arranges its reset on close and sends it on reuse; PgBouncer normally resets on release in session pooling, and normally does not in transaction pooling. Consider changing reset behavior only after verifying that every caller safely handles retained session state."#,
    ),
    (
        "measure/sql-server",
        r#"fix: Read a retained actual plan when the server has one; otherwise capture a representative actual plan in a test environment. Check how many rows each step reads and keeps, and collect IO measurements for the same run to locate the reads on shop.customers. Use that evidence to decide whether a change to statistics or the access path is justified."#,
    ),
    (
        "measure/postgres",
        r#"fix: Capture an actual execution plan for a representative run in a test environment. Check how many rows each step reads and keeps, and collect IO measurements for the same run to locate the reads on shop.customers. Use that evidence to decide whether a change to statistics or the access path is justified."#,
    ),
    (
        "verify/query-store",
        r#"measure: Use an actual plan to check row counts, and collect IO measurements for the same run to locate the reads. After a change supported by that evidence, compare equivalent runs: pages read and time per run should fall without changing the result.
storage: Compare I/O measurements and execution time for equivalent useful work, parameters and load before and after a justified change; retain it only when the measurements show an improvement without changing results.
recorded-wait: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change.
cheaper-runs: After the change ships, re-scan: this query's pages read and time per run should drop, and its share of database time with them.
fewer-runs: After the change ships, re-scan: this query should run fewer times and take a smaller share of database time, while what one run reads and takes can stay as it is, or rise.
counter: Compare time and any avoidable calls under equivalent workload, checking that numbering, metadata updates, returned values and transaction guarantees remain correct. Required atomic calls need not decrease.
session: Compare avoidable session-call overhead for equivalent useful work. Verify transaction guarantees and session isolation; required calls need not decrease.
average: Re-scan after the change: its average duration drops toward what its reads take.
time-or-stops: Re-scan after the change: this statement's time drops or it stops running."#,
    ),
    (
        "verify/slow-log",
        r#"measure: Use an actual plan to check row counts, and collect IO measurements for the same run to locate the reads. After a change supported by that evidence, compare equivalent runs: rows examined and time per run should fall without changing the result.
storage: Compare I/O measurements and execution time for equivalent useful work, parameters and load before and after a justified change; retain it only when the measurements show an improvement without changing results.
recorded-wait: Compare recorded wait totals and execution time for equivalent useful work, parameters and load after the change.
cheaper-runs: After the change ships, re-scan: this query's rows examined and time per run should drop, and its share of logged-run time with them.
fewer-runs: After the change ships, re-scan: this query should run fewer times and take a smaller share of logged-run time, while what one run reads and takes can stay as it is, or rise.
counter: Compare time and any avoidable calls under equivalent workload, checking that numbering, metadata updates, returned values and transaction guarantees remain correct. Required atomic calls need not decrease.
session: Compare avoidable session-call overhead for equivalent useful work. Verify transaction guarantees and session isolation; required calls need not decrease.
average: Re-scan after the change: its average duration drops toward what its reads take.
time-or-stops: Re-scan after the change: this statement's time drops or it stops running."#,
    ),
    (
        "verify/pg-stat-statements",
        r#"measure: Use an actual plan to check row counts, and collect IO measurements for the same run to locate the reads. After a change supported by that evidence, compare equivalent runs: pages read and time per run should fall without changing the result.
storage: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there I/O measurements and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
recorded-wait: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there recorded wait totals and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
cheaper-runs: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
fewer-runs: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
counter: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare time and avoidable calls for equivalent work, preserving numbering, metadata updates, returned values and transaction guarantees. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
session: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare avoidable session-call overhead for equivalent useful work, preserving transaction guarantees and session isolation; required calls need not decrease. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
average: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop toward what its reads take. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
time-or-stops: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time should drop, or it should stop running. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "verify/performance-schema",
        r#"measure: Use an actual plan to check row counts, and collect IO measurements for the same run to locate the reads. After a change supported by that evidence, compare equivalent runs: rows examined and time per run should fall without changing the result.
storage: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there I/O measurements and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
recorded-wait: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there recorded wait totals and execution time should improve under equivalent useful work, parameters and load. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
cheaper-runs: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
fewer-runs: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there it should run fewer times and take less time in all, while a run's own time can stay as it is, or rise. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
counter: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare time and avoidable calls for equivalent work, preserving numbering, metadata updates, returned values and transaction guarantees. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
session: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare avoidable session-call overhead for equivalent useful work, preserving transaction guarantees and session isolation; required calls need not decrease. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
average: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop toward what its reads take. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running.
time-or-stops: performance_schema keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time should drop, or it should stop running. A rewritten statement is a new entry with an id of its own, and this one then shows as stopped running."#,
    ),
    (
        "verify/session-statement",
        r#"pg_stat_statements keeps running totals, so a later scan's runs, share and averages for this query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare avoidable session-call overhead for equivalent useful work, preserving transaction guarantees and session isolation; required calls need not decrease."#,
    ),
    (
        "info",
        r#"title: Queries that dominate database time
description: Single queries that take a large share of database time, with what their plan spends it on."#,
    ),
];
