//! Plan import and comparison on plans captured from the demo databases (dev/mssql, dev/postgres, dev/mysql).

use culprit_core::{ChangeKind, Engine, ErrorCode, NodeKind, Plan, PlanKind, PlanNode, plan_diff, plan_diff_named};
use culprit_engine::plans::{PlanFormat, decode, detect, parse_plan};

#[test]
fn native_docdb_plans_are_imported_as_yugabyte_with_total_rows_and_native_metrics() {
    let text = include_str!("../../providers/culprit-yugabyte/tests/data/loops.json");
    assert_eq!(detect(text), Some(PlanFormat::YugabyteJson));
    let plan = parse_plan(text).unwrap();
    assert_eq!(plan.engine, Engine::Yugabyte);
    assert_eq!(plan.kind, PlanKind::Actual);
    assert_eq!(plan.root.children[1].rows_read_total(), Some(20.0));
    assert_eq!(plan.facts().docdb.as_ref().unwrap().read_requests, Some(21.0));
    assert!(plan.root.children[1].table_rows.is_none());
    let imported = parse_plan(&serde_json::to_string(&plan).unwrap()).unwrap();
    assert_eq!(imported.engine, Engine::Yugabyte);
    assert_eq!(imported.root.children[1].rows_read_total(), Some(20.0));
    assert_eq!(imported.facts().docdb, plan.facts().docdb);
}

// A plan another version of Culprit saved isn't read: what's missing from it says so, and where to go instead.
#[test]
fn a_plan_another_version_saved_says_so() {
    let plan = parse_plan(&fixture("mssql-seek.sqlplan")).unwrap();
    let mut older = serde_json::to_value(&plan).unwrap();
    older["root"].as_object_mut().unwrap().remove("facts");
    let text = serde_json::to_string(&older).unwrap();
    assert_eq!(detect(&text), Some(PlanFormat::Culprit));
    let refused = parse_plan(&text).unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);
    assert!(refused.message.contains("saved by another version of Culprit"), "{}", refused.message);
    assert!(refused.message.contains("engine's own plan text"), "{}", refused.message);
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/plans/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn find<'a>(plan: &'a Plan, op: &str) -> &'a PlanNode {
    plan.nodes().into_iter().find(|n| n.op == op).unwrap_or_else(|| panic!("no {op} in {:?}", ops(plan)))
}

fn ops(plan: &Plan) -> Vec<String> {
    plan.nodes().iter().map(|n| n.op.clone()).collect()
}

#[test]
fn reads_sql_server_plans() {
    let text = fixture("mssql-seek.sqlplan");
    assert_eq!(detect(&text), Some(PlanFormat::ShowplanXml));
    let plan = parse_plan(&text).unwrap();
    assert_eq!(plan.engine, Engine::Mssql);
    assert_eq!(plan.kind, PlanKind::Estimated);
    let seek = find(&plan, "Index Seek");
    assert_eq!(seek.index.as_deref(), Some("ix_customers_email"));
    assert_eq!(plan.raw.as_deref().map(str::trim), Some(text.trim()));

    let actual = parse_plan(&fixture("mssql-actual.sqlplan")).unwrap();
    assert_eq!(actual.kind, PlanKind::Actual);
    assert!(actual.root.act_rows.is_some());
}

#[test]
fn reads_sqlplan_files_saved_as_utf16() {
    let text = fixture("mssql-scan.sqlplan");
    let declared = format!("<?xml version=\"1.0\" encoding=\"utf-16\"?>{}", text.trim());
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(declared.encode_utf16().flat_map(u16::to_le_bytes));
    let decoded = decode(&bytes).unwrap();
    assert!(decoded.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
    let plan = parse_plan(&decoded).unwrap();
    assert!(plan.nodes().iter().any(|n| n.op == "Index Scan"));
    assert_eq!(decode(b"\xEF\xBB\xBFhello").unwrap(), "hello");
    assert_eq!(decode(&[0xC3, 0x28]).unwrap_err().code, ErrorCode::InvalidInput);
}

#[test]
fn pasted_showplans_allow_unicode_across_the_declaration_probe_boundary() {
    let text = fixture("mssql-seek.sqlplan");
    for encoding in ["utf-8", "utf-16"] {
        for quote in ['\'', '"'] {
            let prefix = format!("<?xml version=\"1.0\" encoding={quote}{encoding}{quote}?><!--");
            let plan = format!("{prefix}{}é -->{text}", " ".repeat(119 - prefix.len()));
            assert!(!plan.is_char_boundary(120));
            assert_eq!(parse_plan(&plan).unwrap().engine, Engine::Mssql);
        }
    }
}

#[test]
fn never_resolves_xml_entities() {
    let xml =
        r#"<?xml version="1.0"?><!DOCTYPE p [<!ENTITY e SYSTEM "file:///etc/passwd">]><ShowPlanXML>&e;</ShowPlanXML>"#;
    let error = parse_plan(xml).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(!error.message.contains("root:"), "{}", error.message);
}

#[test]
fn reads_postgres_json_and_psql_wrapped_json() {
    let text = fixture("pg-index.json");
    assert_eq!(detect(&text), Some(PlanFormat::PostgresJson));
    let plan = parse_plan(&text).unwrap();
    assert_eq!(plan.engine, Engine::Postgres);
    assert_eq!(plan.kind, PlanKind::Estimated);
    assert_eq!(plan.root.op, "Limit");
    let scan = find(&plan, "Index Scan");
    assert_eq!(scan.index.as_deref(), Some("orders_status_created_idx"));

    // psql's aligned output: header, dashes, " +" continuations, a leading space and the row count.
    let mut wrapped = String::from("                QUERY PLAN\n------------------------------\n");
    for line in text.trim().lines() {
        wrapped.push_str(&format!(" {line:<60}+\n"));
    }
    wrapped.push_str("(1 row)\n");
    let again = parse_plan(&wrapped).unwrap();
    assert_eq!(ops(&again), ops(&plan));

    // pgAdmin / psql -A style: [{"QUERY PLAN": [...]}].
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let grid = serde_json::json!([{ "QUERY PLAN": value }]).to_string();
    assert_eq!(ops(&parse_plan(&grid).unwrap()), ops(&plan));
}

#[test]
fn reads_postgres_text_explain_analyze() {
    let text = fixture("pg-analyze.txt");
    assert_eq!(detect(&text), Some(PlanFormat::PostgresText));
    let plan = parse_plan(&text).unwrap();
    assert_eq!(plan.kind, PlanKind::Actual);
    assert_eq!(ops(&plan), ["Limit", "Nested Loop", "Index Scan", "Memoize", "Index Scan"]);
    let orders = &plan.root.children[0].children[0];
    assert_eq!(orders.object.as_deref(), Some("orders"));
    assert_eq!(orders.index.as_deref(), Some("orders_status_created_idx"));
    assert!(orders.detail.as_deref().unwrap_or("").contains("Backward"));
    assert_eq!(orders.seek_predicate.as_deref(), Some("(status = 'shipped'::text)"));
    assert_eq!(orders.act_rows, Some(20.0));
    let customers = &plan.root.children[0].children[1].children[0];
    assert_eq!(customers.index.as_deref(), Some("customers_pkey"));
    assert_eq!(customers.act_executions, Some(20.0));
    assert_eq!(plan.props.get("Execution Time").map(String::as_str), Some("1.444"));
    assert!((plan.total_cost - 15.52).abs() < 1e-9);
}

#[test]
fn reads_postgres_text_with_subplans_and_never_executed_nodes() {
    let plan = parse_plan(&fixture("pg-subplans.txt")).unwrap();
    assert_eq!(plan.root.op, "Aggregate");
    let init = &plan.root.children[0];
    assert_eq!(init.op, "Aggregate");
    assert_eq!(init.props.get("Subplan Name").map(String::as_str), Some("InitPlan 1"));
    assert_eq!(init.act_rows, Some(0.0));
    let scan = find(&plan, "Seq Scan");
    assert_eq!(scan.object.as_deref(), Some("public.orders"));
    assert!(scan.detail.as_deref().unwrap_or("").contains("Parallel"));
    assert_eq!(plan.root.children[1].op, "Sort");
    let hash_agg = find(&plan, "Aggregate");
    assert!(hash_agg.detail.is_some());

    let estimated = parse_plan(&fixture("pg-seqscan.txt")).unwrap();
    assert_eq!(estimated.kind, PlanKind::Estimated);
    assert!(estimated.nodes().iter().any(|n| n.op == "Seq Scan" && n.kind == NodeKind::Scan));
}

#[test]
fn reads_mysql_json_tree_and_version_2() {
    let v1 = parse_plan(&fixture("mysql-index.json")).unwrap();
    assert_eq!(v1.engine, Engine::Mysql);
    assert!(v1.nodes().iter().any(|n| n.index.as_deref() == Some("ix_orders_status_created")));

    let v2_text = fixture("mysql-v2.json");
    assert_eq!(detect(&v2_text), Some(PlanFormat::MysqlJson));
    let v2 = parse_plan(&v2_text).unwrap();
    assert_eq!(v2.kind, PlanKind::Estimated);
    let orders = v2.nodes().into_iter().find(|n| n.index.as_deref() == Some("ix_orders_status_created")).unwrap();
    assert_eq!(orders.object.as_deref(), Some("shop.orders"));
    assert!(v2.total_cost > 0.0);

    let analyze = parse_plan(&fixture("mysql-analyze.txt")).unwrap();
    assert_eq!(analyze.kind, PlanKind::Actual);
    assert_eq!(analyze.root.op, "Limit");

    let tree = fixture("mysql-tree-g.txt");
    assert!(tree.contains("EXPLAIN: ->"));
    assert_eq!(detect(&tree), Some(PlanFormat::MysqlTree));
    let tree = parse_plan(&tree).unwrap();
    assert_eq!(tree.kind, PlanKind::Estimated);
    assert_eq!(ops(&tree), ops(&analyze));
}

#[test]
fn round_trips_culprit_json() {
    let plan = parse_plan(&fixture("mssql-scan.sqlplan")).unwrap();
    let mut exported = serde_json::to_value(&plan).unwrap();
    exported["root"]["children"][0]["id"] = serde_json::json!(0);
    let text = exported.to_string();
    assert_eq!(detect(&text), Some(PlanFormat::Culprit));
    let back = parse_plan(&text).unwrap();
    let ids: Vec<u32> = back.nodes().iter().map(|n| n.id).collect();
    assert_eq!(ids, (0..ids.len() as u32).collect::<Vec<_>>());
    assert_eq!(ops(&back), ops(&plan));
}

// What the sqlite3 shell prints for EXPLAIN QUERY PLAN: its tree under the `QUERY PLAN` line psql also heads a plan
// with, and the rows `.mode list` prints with or without their header. psql's own output still reads as PostgreSQL
// (reads_postgres_text_explain_analyze).
#[test]
fn reads_the_sqlite_shells_plans() {
    let tree = "QUERY PLAN\n|--SCAN o\n`--USE TEMP B-TREE FOR ORDER BY\n";
    let list = "id|parent|notused|detail\n2|0|0|SCAN o\n9|0|0|USE TEMP B-TREE FOR ORDER BY\n";
    let rows = list.split_once('\n').unwrap().1;
    for text in [tree, list, rows] {
        assert_eq!(detect(text), Some(PlanFormat::SqliteText), "{text}");
        let plan = parse_plan(text).unwrap_or_else(|e| panic!("{text}: {}", e.message));
        assert_eq!(plan.engine, Engine::Sqlite);
        assert_eq!(plan.root.children.len(), 2, "{text}");
    }
}

#[test]
fn explains_what_it_cannot_read() {
    for (input, needle) in [
        ("hello world", "doesn't recognize"),
        ("<html><body/></html>", "isn't a SQL Server execution plan"),
        ("- Plan:\n    Node Type: \"Seq Scan\"", "XML or YAML"),
        ("{\"rows\": []}", "isn't a plan"),
        ("  |--Clustered Index Scan(OBJECT:([db].[dbo].[t].[pk]))", "SHOWPLAN_TEXT"),
        ("", "no plan"),
    ] {
        let error = parse_plan(input).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput, "{input}");
        assert!(error.message.contains(needle), "{input}: {}", error.message);
    }
    let broken = parse_plan("<ShowPlanXML><unclosed></ShowPlanXML>").unwrap_err();
    assert!(broken.message.starts_with("This looks like SQL Server showplan XML"), "{}", broken.message);
}

#[test]
fn compares_a_seek_with_the_scan_an_implicit_conversion_forces() {
    let seek = parse_plan(&fixture("mssql-seek.sqlplan")).unwrap();
    let scan = parse_plan(&fixture("mssql-scan.sqlplan")).unwrap();
    let diff = plan_diff(&seek, &scan);
    assert!(diff.comparable);
    assert!(!diff.same_shape);
    assert!(diff.only_a.is_empty() && diff.only_b.is_empty(), "{diff:#?}");
    let access = diff.changes.iter().find(|c| c.kind == ChangeKind::Access).expect("an access change");
    assert_eq!(access.title, "customers: Index Seek (ix_customers_email) → Index Scan (ix_customers_email)");
    assert!(
        diff.headlines.iter().any(|h| h
            == "Plan B is estimated 151× more expensive because it scans customers (88% of its cost) where plan A seeks \
                ix_customers_email."),
        "{:?}",
        diff.headlines
    );
    assert!(
        diff.warnings_added.iter().any(|w| matches!(w, culprit_core::PlanWarning::ImplicitConversion))
            || diff.headlines.len() >= 2
    );

    let named = plan_diff_named(&seek, "Plan 815", &scan, "Plan 685");
    assert!(
        named.headlines[0].starts_with("Plan 685 is estimated")
            && named.headlines[0].contains("because it scans customers"),
        "{:?}",
        named.headlines
    );
}

#[test]
fn compares_postgres_index_and_sequential_plans() {
    let index = parse_plan(&fixture("pg-index.json")).unwrap();
    let seq = parse_plan(&fixture("pg-seqscan.json")).unwrap();
    let diff = plan_diff(&index, &seq);
    assert!(diff.cost_ratio.unwrap() > 10.0);
    assert!(diff.headlines[0].contains("more expensive"), "{:?}", diff.headlines);
    assert!(
        diff.changes.iter().any(|c| c.kind == ChangeKind::Access && c.title.starts_with("orders:")),
        "{:#?}",
        diff.changes
    );

    let same = plan_diff(&index, &index);
    assert!(same.same_shape && same.changes.is_empty());
    assert_eq!(same.headlines, ["Plan B runs the same steps as plan A."]);

    let mysql = parse_plan(&fixture("mysql-index.json")).unwrap();
    let cross = plan_diff(&index, &mysql);
    assert!(!cross.comparable && cross.cost_ratio.is_none());
    assert!(cross.headlines[0].contains("different engines"));
}

fn saved_plan<'a>(snapshot: &'a culprit_core::Snapshot, id: &str) -> &'a Plan {
    let q = snapshot.workload.as_ref().unwrap().queries.iter().find(|q| q.id == id).expect("the statement");
    q.plan.as_ref().expect("its plan")
}

#[test]
fn the_demo_scan_carries_its_plans_as_todays_parsers_read_them() {
    // The parallel sort's rows are the whole plan's (666,665), not one process's (277,777).
    let home = std::env::temp_dir().join(format!("culprit-demo-plans-{}", std::process::id()));
    let ws = culprit_engine::Workspace::open_in(&home);
    let path = std::path::PathBuf::from(format!("{}/../../fixtures/postgres-demo.json", env!("CARGO_MANIFEST_DIR")));
    let report = ws.analyze_file(&path).expect("analyze");
    let plan = saved_plan(&report.snapshot, "9065391607317686058");
    assert!((find(plan, "Sort").est_rows - 666_665.0).abs() < 1.0);
    // The big-sort insight rested on the guess: once the scan says so, no plan finding states a share of it.
    assert!(
        !report.findings.iter().any(|f| f.id.starts_with("plan-insights:big-sort")),
        "{:?}",
        report.findings.iter().map(|f| &f.id).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(home);
}
