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
    insights(plan).into_iter().find(|insight| insight.rule == "memory-grant").expect("native memory-grant insight")
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

#[test]
fn native_admission_delay_does_not_identify_a_competing_request_or_an_oversized_reservation() {
    for granted in [4096.0, 8192.0, 12288.0] {
        let mut plan = actual(Engine::Mssql, measured(0, "Sort", NodeKind::Sort, 100.0, 100.0, 1.0), 5000.0);
        plan.facts_mut().memory_requested_kb = Some(8192.0);
        plan.facts_mut().memory_granted_kb = Some(granted);
        plan.facts_mut().memory_max_used_kb = Some(granted);
        plan.facts_mut().memory_grant_wait_ms = Some(3000.0);
        let insight = grant(&plan);
        assert_eq!(insight.title, t!("insight.memory_grant.wait.title", time = "3.0 s"));
        assert_eq!(insight.detail, t!("insight.memory_grant.wait.detail"));
        assert_eq!(insight.fix, t!("insight.memory_grant.wait.fix"));
        assert_eq!(insight.verify, t!("insight.memory_grant.wait.verify"));
        assert!(insight.fix_sql.is_none());
        assert!(!insight.detail.contains("Other queries held"));
        assert!(!insight.fix.contains("adding memory only postpones"));
        assert!(!insight.verify.contains("GrantWaitTime is 0"));
        assert_eq!(plan.facts().memory_grant_wait_ms, Some(3000.0));
        assert_eq!(plan.facts().memory_max_used_kb, Some(granted));
    }
}

#[test]
fn generic_reservation_difference_preserves_native_warning_without_inventing_its_cause_or_feedback_eligibility() {
    for version in [None, Some("14.0"), Some("16.0")] {
        for warning in [None, Some("Excessive Grant"), Some("Grant Increase")] {
            let mut plan = overestimated_input();
            plan.root.children[0].facts_mut().relational_input = None;
            plan.facts_mut().server_version = version.map(str::to_owned);
            plan.facts_mut().memory_grant_warning = warning.map(str::to_owned);
            let insight = grant(&plan);
            assert_eq!(insight.title, t!("insight.memory_grant.oversized.title", granted = "32.0 MB", used = "128 KB"));
            assert!(insight.detail.starts_with(&t!("insight.memory_grant.oversized.detail")));
            if let Some(warning) = warning {
                assert!(insight.detail.contains(&t!("insight.memory_grant.oversized.warned", warning = warning)));
            }
            assert_eq!(insight.fix, t!("insight.memory_grant.oversized.fix"));
            assert_eq!(insight.verify, t!("insight.memory_grant.oversized.verify"));
            assert!(insight.fix_sql.is_none());
            for unproved in ["RESOURCE_SEMAPHORE", "update statistics", "SQL Server 2019", "MAX_GRANT_PERCENT"] {
                assert!(!format!("{} {}", insight.detail, insight.fix).contains(unproved), "{unproved}");
            }
            assert_eq!(plan.facts().memory_granted_kb, Some(32768.0));
            assert_eq!(plan.facts().memory_max_used_kb, Some(128.0));
            assert!(plan.facts().memory_grant_feedback.is_none());
        }
    }
}

#[test]
fn absent_zero_or_short_native_wait_does_not_create_an_admission_delay_diagnosis() {
    for wait in [None, Some(0.0), Some(99.0)] {
        let mut plan = actual(Engine::Mssql, measured(0, "Sort", NodeKind::Sort, 100.0, 100.0, 1.0), 5000.0);
        plan.facts_mut().memory_granted_kb = Some(32768.0);
        plan.facts_mut().memory_max_used_kb = Some(32768.0);
        plan.facts_mut().memory_grant_wait_ms = wait;
        assert!(!insights(&plan).iter().any(|insight| insight.rule == "memory-grant"));
    }
}

#[test]
fn efficient_recorded_use_does_not_create_a_reservation_imbalance_from_estimate_mismatch_alone() {
    for used in [24576.0, 32768.0] {
        let mut plan = overestimated_input();
        plan.facts_mut().memory_max_used_kb = Some(used);
        assert!(!insights(&plan).iter().any(|insight| insight.rule == "memory-grant"));
        assert_eq!(plan.facts().memory_granted_kb, Some(32768.0));
    }
}

#[test]
fn estimated_desire_does_not_become_an_actual_reservation_or_a_pressure_verdict() {
    let mut sort = node(0, "Sort", NodeKind::Sort);
    sort.est_rows = 2_000_000.0;
    let mut planned = plan(Engine::Mssql, sort);
    planned.facts_mut().memory_desired_kb = Some(2000000.0);
    assert!(does_real_work(&planned), "an expensive estimate cannot supply missing pressure evidence");
    let insight = grant(&planned);
    assert_eq!(insight.severity, Severity::Low);
    assert_eq!(insight.title, t!("insight.memory_grant.desired.title", size = "1.9 GB"));
    assert_eq!(insight.detail, t!("insight.memory_grant.desired.detail"));
    assert_eq!(insight.fix, t!("insight.memory_grant.desired.fix"));
    assert_eq!(insight.verify, t!("insight.memory_grant.desired.verify"));
    assert!(insight.fix_sql.is_none());
    assert!(!insight.fix.contains("MAX_GRANT_PERCENT"));
    assert!(planned.facts().memory_granted_kb.is_none());
    planned.kind = PlanKind::Actual;
    assert!(!insights(&planned).iter().any(|insight| insight.rule == "memory-grant"));
}

#[test]
fn sql_server_memory_values_cannot_create_grant_diagnoses_on_other_engines() {
    for engine in Engine::ALL.iter().copied().filter(|engine| *engine != Engine::Mssql) {
        let mut plan = overestimated_input();
        plan.engine = engine;
        plan.facts_mut().memory_grant_wait_ms = Some(3000.0);
        assert!(!insights(&plan).iter().any(|insight| insight.rule == "memory-grant"), "{engine:?}");
    }
}
