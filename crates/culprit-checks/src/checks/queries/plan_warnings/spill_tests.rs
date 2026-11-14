use super::*;
use culprit_core::{Check, NodeKind, PlanKind, actual::NodeRuntime, plan_facts::*};

fn snapshot(kind: NodeKind, op: &str) -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    let mut plan = snapshot.queries().iter().find_map(|query| query.plan.clone()).unwrap();
    plan.kind = PlanKind::Actual;
    plan.warnings.clear();
    plan.facts = Default::default();
    plan.facts.memory_requested_kb = Some(8192.0);
    plan.facts.memory_granted_kb = Some(512.0);
    plan.root.id = 41;
    plan.root.kind = kind;
    plan.root.op = op.into();
    plan.root.object = None;
    plan.root.children.clear();
    plan.root.warnings = vec![PlanWarning::Spill];
    plan.root.facts = Default::default();
    plan.root.est_rows = 10_000.0;
    plan.root.act_rows = Some(10_000.0);
    plan.root.est_executions = 1.0;
    plan.root.act_executions = Some(1.0);
    plan.root.runtime = Some(NodeRuntime { spill_pages: Some(640.0), ..Default::default() });
    let mut query = crate::support::tests::sample();
    query.text = "SELECT id FROM dbo.orders".into();
    query.executions = 1000.0;
    query.total_time_ms = 100_000.0;
    query.mean_time_ms = 100.0;
    query.plan = Some(plan);
    let workload = snapshot.workload.as_mut().unwrap();
    workload.total_time_ms = query.total_time_ms;
    workload.queries = vec![query];
    snapshot
}

fn spill(snapshot: &Snapshot) -> Finding {
    PlanWarningsCheck
        .run(snapshot)
        .into_iter()
        .find(|finding| finding.facts.cause == Some(culprit_core::FindingCause::Spill))
        .unwrap()
}

#[test]
fn the_finding_reuses_qualified_native_spill_guidance_size_and_verification() {
    let snapshot = snapshot(NodeKind::Sort, "Sort");
    let plan = snapshot.queries()[0].plan.as_ref().unwrap();
    let native = culprit_core::insights::insights(plan).into_iter().find(|insight| insight.rule == "spill").unwrap();
    let finding = spill(&snapshot);
    assert!(finding.title.contains("5.0 MB"));
    assert_eq!(finding.summary, native.detail);
    assert_eq!(finding.fix.as_ref().unwrap().summary, native.fix);
    assert_eq!(finding.verify, [native.verify]);
    assert!(finding.fix.unwrap().sql.is_none());
    assert_eq!(finding.queries[0].plan_node, Some(41));
    assert_eq!(plan.facts.memory_granted_kb, Some(512.0));
}

#[test]
fn hash_spills_above_native_tvp_and_local_inputs_cannot_get_statistics_or_sort_cures() {
    for input in [
        RelationalInput {
            kind: RelationalInputKind::TableValuedParameter,
            provenance: RelationalInputProvenance::NativeParameterMetadata,
        },
        RelationalInput {
            kind: RelationalInputKind::LocalTableVariable,
            provenance: RelationalInputProvenance::NativeDeclaration,
        },
    ] {
        let mut snapshot = snapshot(NodeKind::Join, "Hash Match");
        let plan = snapshot.workload.as_mut().unwrap().queries[0].plan.as_mut().unwrap();
        let mut rows = plan.root.clone();
        rows.id = 42;
        rows.op = "Table Scan".into();
        rows.kind = NodeKind::Scan;
        rows.object = Some("@input".into());
        rows.warnings.clear();
        rows.runtime = None;
        rows.est_rows = 100.0;
        rows.facts.relational_input = Some(input);
        plan.root.children.push(rows);
        let finding = spill(&snapshot);
        assert!(finding.summary.contains("mismatch alone does not establish"));
        let fix = finding.fix.unwrap();
        assert!(fix.sql.is_none());
        assert!(!fix.summary.contains("statistics") && !fix.summary.contains("required order"));
        assert!(!fix.summary.contains("150") && !fix.summary.contains("2019"));
    }
}

#[test]
fn a_plan_only_warning_preserves_the_symptom_without_inventing_operator_cause_or_a_mutation() {
    for engine in [Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql] {
        let mut snapshot = snapshot(NodeKind::Other, "Unresolved operator");
        snapshot.server.engine = engine;
        let plan = snapshot.workload.as_mut().unwrap().queries[0].plan.as_mut().unwrap();
        plan.engine = engine;
        plan.root.warnings.clear();
        plan.root.runtime = None;
        plan.warnings = vec![PlanWarning::Spill];
        let finding = spill(&snapshot);
        assert!(finding.summary.contains("does not establish its size, elapsed cost or cause"));
        assert_eq!(finding.queries[0].plan_node, None);
        assert!(finding.fix.as_ref().unwrap().sql.is_none());
        assert!(finding.fix.unwrap().summary.contains("Establish the mechanism before changing"));
        assert!(finding.verify[0].contains("owned test environment"));
        assert!(!finding.verify[0].contains("should be gone"));
    }
}

#[test]
fn the_selected_native_operator_cannot_borrow_another_spills_sort_advice() {
    let mut snapshot = snapshot(NodeKind::Join, "Hash Match");
    let plan = snapshot.workload.as_mut().unwrap().queries[0].plan.as_mut().unwrap();
    let mut sort = plan.root.clone();
    sort.id = 42;
    sort.kind = NodeKind::Sort;
    sort.op = "Sort".into();
    plan.root.children.push(sort);
    let finding = spill(&snapshot);
    assert_eq!(finding.queries[0].plan_node, Some(41));
    assert!(finding.title.contains("Hash Match"));
    assert!(!finding.fix.unwrap().summary.contains("required order"));
}
