use super::*;
use culprit_core::actual::page_server_io::{PlanPageServerIoBuilder, PlanPageServerThread};
use culprit_core::{Engine, NodeRuntime, Plan, PlanKind, Unit};

fn plan() -> Plan {
    let snapshot = crate::waits::snapshot_with(Engine::Mssql);
    let mut plan = snapshot.queries().iter().find_map(|query| query.plan.clone()).unwrap();
    plan.kind = PlanKind::Actual;
    plan.root.children.clear();
    plan.root.runtime = None;
    plan
}
fn counters(reads: Option<&str>) -> Box<PlanPageServerIo> {
    let mut builder = PlanPageServerIoBuilder::default();
    builder.observe(PlanPageServerThread {
        thread: Some("0"),
        reads,
        read_aheads: Some("0"),
        lob_reads: Some("18446744073709551615"),
        lob_read_aheads: None,
    });
    Box::new(builder.finish().unwrap())
}

#[test]
fn exact_native_metrics_and_unknowns_are_rendered_once_without_loss_or_a_numeric_shadow() {
    let mut plan = plan();
    plan.root.runtime =
        Some(NodeRuntime { page_server_io: Some(counters(Some("9007199254740993"))), ..Default::default() });
    let view = super::super::plan_view(&plan, None);
    let io = view.nodes.iter().find(|node| node.id == plan.root.id).unwrap().page_server_io.as_ref().unwrap();
    assert_eq!(io.metrics.len(), 4);
    assert!(io.metrics.iter().all(|metric| metric.unit == Unit::Text && metric.value == 0.0));
    assert_eq!(io.metrics[0].text.as_deref(), Some("9007199254740993"));
    assert_eq!(io.metrics[1].text.as_deref(), Some("0"));
    assert_eq!(io.metrics[2].text.as_deref(), Some("18446744073709551615"));
    assert_eq!(io.metrics[3].text, Some(t!("view.plan_page_server_io.unknown")));
    assert_eq!(
        io.limitations,
        [t!("view.plan_page_server_io.limit.recorded"), t!("view.plan_page_server_io.limit.partial")]
    );
}

#[test]
fn operators_keep_their_own_native_counts_without_a_plan_total_or_current_execution_claim() {
    let mut plan = plan();
    plan.root.runtime = Some(NodeRuntime { page_server_io: Some(counters(Some("100"))), ..Default::default() });
    let mut child = plan.root.clone();
    child.id = plan.root.id + 1;
    child.runtime.as_mut().unwrap().page_server_io = Some(counters(Some("200")));
    plan.root.children.push(child);
    let view = super::super::plan_view(&plan, None);
    let parent = view.nodes.iter().find(|node| node.id == plan.root.id).unwrap();
    let child = view.nodes.iter().find(|node| node.id != plan.root.id).unwrap();
    assert_eq!(parent.page_server_io.as_ref().unwrap().metrics[0].text.as_deref(), Some("100"));
    assert_eq!(child.page_server_io.as_ref().unwrap().metrics[0].text.as_deref(), Some("200"));
    assert_eq!(view.actual.unwrap().physical_reads, None);
    assert!(view.query.is_none());
}

#[test]
fn ordinary_plans_have_no_empty_remote_panel_or_disclaimer() {
    let mut plan = plan();
    for kind in [PlanKind::Actual, PlanKind::Estimated] {
        plan.kind = kind;
        let view = super::super::plan_view(&plan, None);
        assert!(view.nodes.iter().all(|node| node.page_server_io.is_none()));
    }
}

#[test]
fn invalid_direct_models_cannot_leak_raw_fields_or_become_positive_metrics() {
    let mut plan = plan();
    let mut invalid = counters(Some("9"));
    invalid.reads = Some("private-saved-counter".into());
    plan.root.runtime = Some(NodeRuntime { page_server_io: Some(invalid), ..Default::default() });
    let view = super::super::plan_view(&plan, None);
    let io = view.nodes[0].page_server_io.as_ref().unwrap();
    assert!(io.metrics.is_empty());
    assert_eq!(io.limitations, [t!("view.plan_page_server_io.invalid")]);
    assert!(!serde_json::to_string(&view).unwrap().contains("private-saved-counter"));
}
