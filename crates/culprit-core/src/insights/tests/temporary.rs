use super::*;
use crate::plan_facts::PlanTemporaryTableUse;

fn temporary(use_: PlanTemporaryTableUse, filesort: Option<bool>) -> PlanNode {
    let mut input = node(3, "Native input", NodeKind::Scan);
    input.est_rows = 100_000.0;
    let mut temporary = node(2, "Native intermediate result", NodeKind::Other);
    temporary.est_rows = 100_000.0;
    temporary.children = vec![input];
    temporary.facts_mut().temporary_table = Some(use_);
    temporary.facts_mut().filesort = filesort;
    temporary
}

fn explanations(plan: &Plan) -> Vec<PlanInsight> {
    insights(plan).into_iter().filter(|insight| insight.rule == "temp-table-sort").collect()
}

#[test]
fn native_grouping_without_filesort_explains_grouping_without_inventing_a_sort_or_spill() {
    let p = plan(Engine::Mysql, temporary(PlanTemporaryTableUse::Grouping, Some(false)));
    assert!(p.statement.is_none());
    let found = explanations(&p);
    assert_eq!(found.len(), 1);
    assert!(found[0].title.starts_with("Groups 100K rows"));
    assert!(found[0].detail.contains("for grouping"));
    assert!(!found[0].title.contains("sort") && !found[0].detail.contains("separate sorting step"));
    assert!(!found[0].detail.contains("tmp_table_size"));
    assert_eq!(found[0].severity, Severity::Low);
    assert!(found[0].fix_sql.is_none());
}

#[test]
fn duplicate_removal_is_not_relabelled_from_an_unrelated_group_by_in_sql() {
    let mut p = plan(Engine::Mysql, temporary(PlanTemporaryTableUse::DuplicateRemoval, Some(false)));
    p.statement = Some("SELECT payload FROM materialized_result /* GROUP BY private_column */".into());
    let found = explanations(&p);
    assert_eq!(found.len(), 1);
    assert!(found[0].title.starts_with("Removes duplicates"));
    assert!(found[0].detail.contains("duplicate removal"));
    assert!(!found[0].detail.contains("for grouping"));
}

#[test]
fn missing_sql_unknown_temporary_use_and_display_strings_cannot_establish_grouping_or_filesort() {
    for statement in [None, Some("SELECT category,COUNT(*) FROM items GROUP BY category")] {
        for filesort in [None, Some(false)] {
            let mut root = temporary(PlanTemporaryTableUse::Unknown, filesort);
            root.op = "Temporary table".into();
            root.detail = Some("using temporary table; using filesort".into());
            root.props.insert("using_filesort".into(), "true".into());
            let mut p = plan(Engine::Mysql, root);
            p.statement = statement.map(str::to_owned);
            assert!(explanations(&p).is_empty());
        }
    }
    let mut unqualified = temporary(PlanTemporaryTableUse::Grouping, Some(true));
    unqualified.facts_mut().temporary_table = None;
    unqualified.op = "Group".into();
    unqualified.detail = Some("using temporary table".into());
    assert!(explanations(&plan(Engine::Mysql, unqualified)).is_empty());
}

#[test]
fn an_unknown_temporary_result_followed_by_native_filesort_keeps_its_purpose_unknown() {
    let temporary = temporary(PlanTemporaryTableUse::Unknown, None);
    let mut sorting = node(1, "Native sorting", NodeKind::Sort);
    sorting.facts_mut().filesort = Some(true);
    sorting.children.push(temporary);
    let p = plan(Engine::Mysql, sorting);
    let found = explanations(&p);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].nodes, vec![2, 1]);
    assert!(found[0].detail.contains("purpose is unresolved"));
    assert!(found[0].detail.contains("does not establish disk I/O"));
    assert!(found[0].fix.contains("selected query block"));
    assert_eq!(found[0].severity, Severity::Low);
}

#[test]
fn known_materialization_is_distinct_and_materialization_alone_has_no_warning() {
    let plain = temporary(PlanTemporaryTableUse::Materialization, None);
    assert!(explanations(&plan(Engine::Mysql, plain.clone())).is_empty());
    let mut sorting = node(1, "Native sorting", NodeKind::Sort);
    sorting.facts_mut().filesort = Some(true);
    sorting.children.push(plain);
    let found = explanations(&plan(Engine::Mysql, sorting));
    assert_eq!(found.len(), 1);
    assert!(found[0].detail.contains("materializes an intermediate result"));
    assert!(!found[0].detail.contains("purpose is unresolved"));
    assert!(found[0].fix.contains("materialized result"));
}

#[test]
fn ordering_of_aggregate_results_does_not_prove_index_order_or_an_order_by_expression() {
    let mut root = temporary(PlanTemporaryTableUse::Ordering, Some(true));
    root.children[0].kind = NodeKind::Aggregate;
    let mut p = plan(Engine::Mysql, root.clone());
    p.statement = Some("SELECT * FROM unrelated /* ORDER BY COUNT(*) DESC */".into());
    let aggregate = explanations(&p);
    assert_eq!(aggregate.len(), 1);
    assert!(aggregate[0].title.starts_with("Sorts aggregate results"));
    assert!(aggregate[0].detail.contains("does not establish that grouping read an index"));
    assert!(!aggregate[0].fix.contains("always needs"));
    root.children[0].kind = NodeKind::Scan;
    let ordinary = explanations(&plan(Engine::Mysql, root));
    assert!(ordinary[0].title.starts_with("Sorts 100K rows"));
    assert!(!ordinary[0].detail.contains("aggregate results"));
}

#[test]
fn one_explanation_covers_the_same_direct_temporary_grouping_and_ordering_chain() {
    let group = temporary(PlanTemporaryTableUse::Grouping, Some(false));
    let mut ordering = node(1, "Ordering", NodeKind::Sort);
    ordering.facts_mut().temporary_table = Some(PlanTemporaryTableUse::Ordering);
    ordering.facts_mut().filesort = Some(true);
    ordering.children = vec![group];
    let found = explanations(&plan(Engine::Mysql, ordering));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].nodes, vec![2, 1]);
    assert!(found[0].title.contains("then sorts the result"));
}

#[test]
fn unrelated_nested_grouping_cannot_classify_the_outer_temporary_ordering() {
    let mut ordering = temporary(PlanTemporaryTableUse::Ordering, Some(true));
    let grouping = temporary(PlanTemporaryTableUse::Grouping, Some(false));
    ordering.children[0].children.push(grouping);
    // Give the nested branch distinct native node IDs. There is no direct aggregate input at the ordering step.
    ordering.children[0].children[0].id = 4;
    ordering.children[0].children[0].children[0].id = 5;
    let found = explanations(&plan(Engine::Mysql, ordering));
    let outer = found.iter().find(|insight| insight.nodes.contains(&2)).unwrap();
    assert!(outer.title.starts_with("Sorts 100K rows"));
    assert!(!outer.title.contains("aggregate results"));
}

#[test]
fn native_zero_work_and_small_results_do_not_create_a_generic_temporary_table_warning() {
    let mut root = temporary(PlanTemporaryTableUse::Grouping, Some(true));
    root.est_rows = 10.0;
    root.children[0].est_rows = 10.0;
    assert!(explanations(&plan(Engine::Mysql, root)).is_empty());
    let mut root = temporary(PlanTemporaryTableUse::Grouping, Some(true));
    root.act_rows = Some(0.0);
    root.act_executions = Some(0.0);
    root.children[0].act_rows = Some(0.0);
    root.children[0].act_executions = Some(0.0);
    assert!(explanations(&actual(Engine::Mysql, root, 0.0)).is_empty());
}

#[test]
fn native_flags_survive_roundtrip_and_are_independent_of_display_properties() {
    let mut p = plan(Engine::Mysql, temporary(PlanTemporaryTableUse::Grouping, Some(false)));
    let expected = serde_json::to_value(explanations(&p)).unwrap();
    p.root.props.insert("using_filesort".into(), "true".into());
    p.root.detail = Some("using filesort, using temporary table".into());
    let restored: Plan = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(restored.root.facts().filesort, Some(false));
    assert_eq!(restored.root.facts().temporary_table, Some(PlanTemporaryTableUse::Grouping));
    assert_eq!(serde_json::to_value(explanations(&restored)).unwrap(), expected);
    let mut malformed = serde_json::to_value(restored.root.facts()).unwrap();
    malformed["temporary_table"] = serde_json::json!("invented-native-purpose");
    assert!(serde_json::from_value::<crate::PlanNodeFacts>(malformed).is_err());
}
