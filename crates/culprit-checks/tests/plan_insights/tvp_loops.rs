use super::*;
use culprit_core::{
    Engine, NodeKind, Plan, PlanKind, PlanNode, Properties,
    plan_facts::{
        PlanAccessRows, PlanScanCompletion, PlanScanThread, RelationalInput, RelationalInputKind,
        RelationalInputProvenance,
    },
};

fn operator(id: u32, kind: NodeKind, op: &str, estimate: f64, actual: f64, executions: u64) -> PlanNode {
    PlanNode {
        facts: culprit_core::PlanNodeFacts {
            serial_execution_count: Some(executions.to_string()),
            ..Default::default()
        },
        id,
        op: op.into(),
        kind,
        detail: None,
        object: None,
        index: None,
        seek_predicate: None,
        predicate: None,
        est_rows: estimate,
        est_executions: 1.0,
        act_rows: Some(actual),
        act_executions: Some(executions as f64),
        rows_read: None,
        table_rows: None,
        subtree_cost: 100.0,
        self_cost: 30.0,
        act_time_ms: Some(500.0),
        warnings: vec![],
        props: Properties::new(),
        children: vec![],
        runtime: None,
    }
}

fn snapshot() -> Snapshot {
    let mut outer = operator(1, NodeKind::Scan, "Table Scan", 100.0, 10_000.0, 1);
    outer.object = Some("@Rows".into());
    outer.facts_mut().relational_input = Some(RelationalInput {
        kind: RelationalInputKind::TableValuedParameter,
        provenance: RelationalInputProvenance::NativeParameterMetadata,
    });
    outer.facts_mut().access_rows = PlanAccessRows::new(Some(100.0), Some(20_000.0));
    outer.facts_mut().scan_completion = Some(PlanScanCompletion {
        parallel: Some(false),
        threads: vec![PlanScanThread {
            thread: 0,
            executions: Some("1".into()),
            end_of_scans: Some("1".into()),
            rebinds: None,
            rewinds: None,
        }],
    });
    let mut inner = operator(2, NodeKind::Seek, "Index Seek", 1.0, 10_000.0, 10_000);
    inner.object = Some("shop.orders".into());
    inner.facts_mut().access_rows = PlanAccessRows::new(None, Some(2_000_000.0));
    let mut join = operator(0, NodeKind::Join, "Nested Loops", 100.0, 10_000.0, 1);
    join.facts_mut().inner_join = Some(true);
    join.children = vec![outer, inner];
    let plan = Plan {
        facts: Default::default(),
        view: None,
        engine: Engine::Mssql,
        kind: PlanKind::Actual,
        statement: Some("SELECT r.id FROM @Rows r JOIN shop.orders o ON r.id = o.id".into()),
        total_cost: 100.0,
        root: join,
        warnings: vec![],
        index_hints: vec![],
        props: Properties::new(),
        raw: None,
        insights: None,
    };
    let mut snapshot = report("mssql-demo.report.json");
    let workload = snapshot.workload.as_mut().unwrap();
    workload.queries.truncate(1);
    let query = &mut workload.queries[0];
    query.plan = Some(plan);
    query.plan_history = None;
    query.plans.clear();
    query.total_time_ms = workload.total_time_ms;
    snapshot
}

#[test]
fn the_shared_plan_insight_check_preserves_exact_tvp_loop_evidence_and_nonexecutable_advice() {
    let snapshot = snapshot();
    let findings = plan_insights(&snapshot);
    let finding = findings.iter().find(|finding| finding.id.contains("nested-loops:")).expect("measured loop");
    assert_eq!(finding.queries.len(), 1);
    assert_eq!(finding.queries[0].id, snapshot.workload.as_ref().unwrap().queries[0].id);
    assert_eq!(finding.queries[0].plan_node, Some(0));
    assert!(finding.summary.contains(&culprit_core::t!("insight.nested_loops.tvp.limit")));
    let fix = finding.fix.as_ref().unwrap();
    assert_eq!(fix.summary, culprit_core::t!("insight.misestimate.fix.tvp", table = "@Rows"));
    assert!(fix.sql.is_none());
    assert!(finding.verify.contains(&culprit_core::t!("insight.nested_loops.tvp.verify")));
}

#[test]
fn an_important_query_does_not_supply_missing_native_inner_access_or_serial_execution_proof() {
    for missing in ["access", "serial"] {
        let mut snapshot = snapshot();
        let plan = snapshot.workload.as_mut().unwrap().queries[0].plan.as_mut().unwrap();
        let inner = &mut plan.root.children[1];
        match missing {
            "access" => {
                inner.facts_mut().access_rows = None;
                inner.rows_read = Some(2_000_000.0);
            }
            "serial" => inner.facts_mut().serial_execution_count = None,
            _ => unreachable!(),
        }
        assert!(!plan_insights(&snapshot).iter().any(|finding| finding.id.contains("nested-loops:")), "{missing}");
    }
}
