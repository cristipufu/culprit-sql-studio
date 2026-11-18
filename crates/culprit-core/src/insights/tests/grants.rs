use super::*;
use crate::plan_facts::{
    PlanAccessRows, PlanScanCompletion, PlanScanThread, RelationalInput, RelationalInputKind, RelationalInputProvenance,
};

fn overestimated_input() -> Plan {
    let mut input = measured(1, "Table Scan", NodeKind::Scan, 100_000.0, 1.0, 1.0);
    input.object = Some("@Rows".into());
    input.facts_mut().relational_input = Some(RelationalInput {
        kind: RelationalInputKind::TableValuedParameter,
        provenance: RelationalInputProvenance::NativeParameterMetadata,
    });
    input.facts_mut().access_rows = PlanAccessRows::new(Some(100_000.0), Some(1.0));
    input.facts_mut().scan_completion = Some(PlanScanCompletion {
        parallel: Some(false),
        threads: vec![PlanScanThread {
            thread: 0,
            executions: Some("1".into()),
            end_of_scans: Some("1".into()),
            rebinds: None,
            rewinds: None,
        }],
    });
    let mut sort = measured(0, "Sort", NodeKind::Sort, 100_000.0, 1.0, 1.0);
    sort.facts_mut().serial_execution_count = Some("1".into());
    sort.children.push(input);
    let mut plan = actual(Engine::Mssql, sort, 100.0);
    plan.facts_mut().memory_granted_kb = Some(32_768.0);
    plan.facts_mut().memory_max_used_kb = Some(128.0);
    plan
}

fn grant(plan: &Plan) -> PlanInsight {
    insights(plan).into_iter().find(|insight| insight.rule == "memory-grant").expect("measured oversized grant")
}

fn linked_input(plan: &Plan) -> bool {
    grant(plan).nodes.contains(&1)
}

#[test]
fn an_oversized_grant_links_the_original_completed_tvp_and_keeps_its_cause_qualified() {
    let plan = overestimated_input();
    let insight = grant(&plan);
    assert_eq!(insight.nodes, vec![0, 1]);
    assert!(insight.detail.contains(&t!(
        "insight.memory_grant.tvp.input",
        input = "@Rows",
        estimated = "100K",
        actual = "1"
    )));
    assert_eq!(insight.fix, t!("insight.memory_grant.tvp.investigate"));
    assert_eq!(insight.verify, t!("insight.memory_grant.tvp.verify"));
    assert!(insight.fix_sql.is_none(), "neither a memory setting nor application replay is authorized");
    assert_eq!(plan.facts().memory_granted_kb, Some(32_768.0));
    assert_eq!(plan.root.children[0].facts().access_rows.unwrap().actual_total, Some(1.0));
}

#[test]
fn missing_input_authority_or_partial_execution_does_not_explain_a_statement_grant() {
    for missing in ["kind", "completion", "unfinished", "parallel", "repeated", "seek", "row-goal", "output"] {
        let mut plan = overestimated_input();
        let input = &mut plan.root.children[0];
        match missing {
            "kind" => input.facts_mut().relational_input = None,
            "completion" => input.facts_mut().scan_completion = None,
            "unfinished" => {
                input.facts_mut().scan_completion.as_mut().unwrap().threads[0].end_of_scans = Some("0".into())
            }
            "parallel" => input.facts_mut().parallel_threads = Some(2),
            "repeated" => input.act_executions = Some(2.0),
            "seek" => input.kind = NodeKind::Seek,
            "row-goal" => input.facts_mut().estimated_rows_without_goal = Some(100_000.0),
            _ => input.act_rows = None,
        }
        assert!(!linked_input(&plan), "{missing}");
        assert_eq!(grant(&plan).fix, t!("insight.memory_grant.oversized.fix"));
    }
}

#[test]
fn a_large_input_accurate_estimate_or_underestimate_cannot_become_the_overestimated_input_case() {
    for (estimate, accessed, output) in
        [(100_000.0, 100_000.0, 100_000.0), (1.0, 100_000.0, 100_000.0), (100_000.0, 100_000.0, 1.0), (99.0, 1.0, 1.0)]
    {
        let mut plan = overestimated_input();
        let input = &mut plan.root.children[0];
        input.est_rows = estimate;
        input.act_rows = Some(output);
        input.facts_mut().access_rows = PlanAccessRows::new(Some(estimate), Some(accessed));
        assert!(!linked_input(&plan), "{estimate}/{accessed}/{output}");
    }
}

#[test]
fn another_memory_consuming_branch_does_not_attach_a_tvp_to_its_grant() {
    let mut plan = overestimated_input();
    let input = plan.root.children.remove(0);
    let mut root = measured(2, "Concatenation", NodeKind::Other, 1.0, 1.0, 1.0);
    root.children = vec![plan.root, input];
    plan.root = root;
    assert!(!linked_input(&plan));
}

#[test]
fn a_tvp_estimate_difference_without_measured_overallocation_creates_no_grant_diagnosis() {
    for (granted, used) in [(None, None), (Some(32_768.0), None), (Some(32_768.0), Some(32_000.0))] {
        let mut plan = overestimated_input();
        plan.facts_mut().memory_granted_kb = granted;
        plan.facts_mut().memory_max_used_kb = used;
        assert!(!insights(&plan).iter().any(|insight| insight.rule == "memory-grant"));
    }
}

#[test]
fn invalid_grant_or_usage_measurements_cannot_establish_memory_overallocation() {
    for invalid in [-1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        for field in ["granted", "used"] {
            let mut plan = overestimated_input();
            if field == "granted" {
                plan.facts_mut().memory_granted_kb = Some(invalid);
            } else {
                plan.facts_mut().memory_max_used_kb = Some(invalid);
            }
            assert!(!insights(&plan).iter().any(|insight| insight.rule == "memory-grant"), "{field}: {invalid}");
        }
    }
}

#[test]
fn invalid_planned_memory_or_wait_time_cannot_become_a_memory_warning() {
    for invalid in [-1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let mut planned = plan(Engine::Mssql, node(0, "Sort", NodeKind::Sort));
        planned.facts_mut().memory_desired_kb = Some(invalid);
        assert!(!insights(&planned).iter().any(|insight| insight.rule == "memory-grant"));
        let mut measured = overestimated_input();
        measured.facts_mut().memory_granted_kb = None;
        measured.facts_mut().memory_grant_wait_ms = Some(invalid);
        assert!(!insights(&measured).iter().any(|insight| insight.rule == "memory-grant"));
    }
}
