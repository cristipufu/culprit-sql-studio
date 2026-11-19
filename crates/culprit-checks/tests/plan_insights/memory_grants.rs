use super::*;
use culprit_core::{NodeKind, PlanKind, Severity};

#[test]
fn estimated_desired_memory_stays_informational_even_for_an_expensive_high_share_query() {
    let mut snapshot = report("mssql-demo.report.json");
    let workload = snapshot.workload.as_mut().unwrap();
    workload.queries.truncate(1);
    let query = &mut workload.queries[0];
    query.total_time_ms = workload.total_time_ms;
    query.plan_history = None;
    query.plans.clear();
    let plan = query.plan.as_mut().unwrap();
    plan.kind = PlanKind::Estimated;
    plan.facts = Default::default();
    plan.facts_mut().memory_desired_kb = Some(2_000_000.0);
    plan.total_cost = 100.0;
    plan.insights = None;
    plan.root.kind = NodeKind::Sort;
    plan.root.op = "Sort".into();
    plan.root.est_rows = 2_000_000.0;
    plan.root.act_rows = None;
    plan.root.act_executions = None;
    plan.root.act_time_ms = None;
    plan.root.runtime = None;
    plan.root.facts = Default::default();
    plan.root.children.clear();
    plan.root.warnings.clear();
    plan.root.self_cost = 100.0;
    plan.root.subtree_cost = 100.0;
    assert!(culprit_core::insights::does_real_work(plan));
    let context = culprit_core::insights::insights(plan)
        .into_iter()
        .find(|insight| insight.rule == "memory-grant")
        .expect("the plan viewer retains the estimated memory context");
    assert_eq!(context.severity, Severity::Low);
    assert_eq!(context.title, culprit_core::t!("insight.memory_grant.desired.title", size = "1.9 GB"));
    assert!(context.fix_sql.is_none());
    assert!(plan_insights(&snapshot).iter().all(|finding| !finding.id.contains("memory-grant:")));
}
