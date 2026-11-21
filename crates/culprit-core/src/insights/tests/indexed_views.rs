use super::*;
use crate::telemetry::MssqlIndexParentKind;
#[test]
fn original_view_kind_withholds_table_index_advice_but_keeps_raw_work() {
    let scan = filtered_scan(1, "Clustered Index Scan", "reporting.Revenue", "customer_id = 42");
    let mut original = plan(Engine::Mssql, scan);
    original.kind = PlanKind::Actual;
    assert!(insights(&original).iter().any(|item| item.rule == "scan-filter"));
    original.root.facts_mut().mssql_index_parent_kind = Some(MssqlIndexParentKind::IndexedView);
    assert!(!insights(&original).iter().any(|item| item.area == InsightArea::Index));
    assert_eq!(original.root.rows_read, Some(100_000.0));
    assert_eq!(original.root.act_rows, Some(10.0));
    let restored: Plan = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert!(!insights(&restored).iter().any(|item| item.area == InsightArea::Index));
    original.engine = Engine::Postgres;
    assert!(
        insights(&original).iter().any(|item| item.rule == "scan-filter"),
        "vendor facts cannot alter other-provider advice"
    );
}
#[test]
fn another_original_schema_or_join_branch_cannot_lend_the_view_kind_to_a_table() {
    let table = filtered_scan(1, "Clustered Index Scan", "dbo.Revenue", "customer_id = 42");
    let mut view = filtered_scan(2, "Clustered Index Scan", "reporting.Revenue", "customer_id = 42");
    view.facts_mut().mssql_index_parent_kind = Some(MssqlIndexParentKind::IndexedView);
    let mut join = node(0, "Nested Loops", NodeKind::Join);
    join.children = vec![table, view];
    let mut original = plan(Engine::Mssql, join);
    original.kind = PlanKind::Actual;
    let found = insights(&original);
    assert!(found.iter().any(|item| item.rule == "scan-filter" && item.nodes.first() == Some(&1)));
    assert!(!found.iter().any(|item| item.rule == "scan-filter" && item.nodes.first() == Some(&2)));
    original.index_hints.push(crate::PlanIndexHint {
        table: "dbo.Revenue".into(),
        equality: vec!["customer_id".into()],
        inequality: vec![],
        include: vec![],
        impact_pct: 90.0,
    });
    let found = insights(&original);
    let hint =
        found.iter().find(|item| item.rule == "missing-index-hint").expect("the table request remains independent");
    assert_eq!(hint.nodes, vec![1]);
    assert_eq!(hint.index_hint.as_ref().unwrap().table, "dbo.Revenue");
}
#[test]
fn current_catalog_enrichment_never_promotes_or_rewrites_original_native_parent_kind() {
    let mut original = plan(Engine::Mssql, filtered_scan(1, "Clustered Index Scan", "dbo.Revenue", "customer_id = 42"));
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.tables.truncate(1);
    snapshot.tables[0].schema = "dbo".into();
    snapshot.tables[0].name = "Revenue".into();
    mark_plan(&mut original, &snapshot);
    assert_eq!(
        original.root.facts().mssql_index_parent_kind,
        None,
        "a current table is not original native object-kind proof"
    );
    original.root.facts_mut().mssql_index_parent_kind = Some(MssqlIndexParentKind::IndexedView);
    mark_plan(&mut original, &snapshot);
    assert_eq!(original.root.facts().mssql_index_parent_kind, Some(MssqlIndexParentKind::IndexedView));
    assert!(!insights(&original).iter().any(|item| item.area == InsightArea::Index));
}

#[test]
fn an_objectless_spool_keeps_its_actual_original_subject_without_borrowing_a_sibling_view() {
    let table = filtered_scan(1, "Clustered Index Scan", "dbo.Orders", "customer_id = 42");
    let mut view = filtered_scan(2, "Clustered Index Scan", "reporting.Revenue", "customer_id = 42");
    view.facts_mut().mssql_index_parent_kind = Some(MssqlIndexParentKind::IndexedView);
    let mut input = node(3, "Nested Loops", NodeKind::Join);
    input.children = vec![table.clone(), view.clone()];
    let mut spool = node(0, "Index Spool", NodeKind::Other);
    spool.detail = Some("Eager Spool".into());
    spool.seek_predicate = Some("customer_id = 42".into());
    spool.children = vec![input];
    let original = plan(Engine::Mssql, spool);
    assert!(insights(&original).iter().any(|item| item.rule == "eager-spool"));
    let mut other = original.clone();
    other.root.children[0].children = vec![view, table];
    assert!(
        !insights(&other).iter().any(|item| item.rule == "eager-spool"),
        "the actual selected view subject gets no table-index DDL; unrelated table input is not a replacement owner"
    );
}
