use super::*;
use crate::plan_facts::{PlanScanCompletion, PlanScanThread};

fn completion() -> PlanScanCompletion {
    PlanScanCompletion {
        parallel: Some(false),
        threads: vec![PlanScanThread {
            thread: 0,
            executions: Some("1".into()),
            end_of_scans: Some("1".into()),
            rebinds: None,
            rewinds: None,
        }],
    }
}

fn completed_input() -> Plan {
    let mut plan = access_plan(100.0, 1000.0, 100.0, 10_000.0);
    plan.root.facts_mut().scan_completion = Some(completion());
    plan
}

fn completed_counts(plan: &Plan) -> String {
    let input = plan.nodes().into_iter().find(|node| node.id == 1).unwrap();
    let access = input.facts().access_rows.unwrap();
    t!(
        "insight.relational_input.completed_input_counts",
        estimated = rows_text(access.estimated_per_execution.unwrap_or(100.0) * input.act_executions.unwrap_or(1.0)),
        actual = rows_text(access.actual_total.unwrap_or(10_000.0))
    )
}

#[test]
fn a_completed_qualified_tvp_scan_distinguishes_consumed_input_kept_rows_and_statement_output() {
    let input = completed_input().root;
    let mut root = measured(0, "Stream Aggregate", NodeKind::Aggregate, 1.0, 1.0, 1.0);
    root.facts_mut().serial_execution_count = Some("1".into());
    root.children.push(input);
    let plan = actual(Engine::Mssql, root, 400.0);
    let concern = estimate_concern(&plan);
    assert!(concern.detail.contains(&completed_counts(&plan)));
    assert!(concern.detail.contains(&t!("insight.relational_input.root_output", actual = "1")));
    assert!(!concern.detail.contains(&t!("insight.relational_input.access_counts", estimated = "100", actual = "10K")));
    assert!(concern.detail.contains(&t!("insight.relational_input.access_and_selectivity")));
    assert!(concern.fix_sql.is_none());
    assert_eq!(concern.nodes, vec![1]);
}

#[test]
fn completed_input_volume_and_filter_selectivity_have_different_evidence_and_investigations() {
    let mut volume = access_plan(100.0, 10_000.0, 100.0, 10_000.0);
    volume.root.facts_mut().scan_completion = Some(completion());
    let volume = estimate_concern(&volume);
    assert!(volume.detail.contains(&t!("insight.relational_input.completed_input_volume")));
    assert!(volume.fix.starts_with(&t!("insight.relational_input.investigate_completed_input")));

    let mut selectivity = access_plan(100.0, 10_000.0, 10_000.0, 10_000.0);
    selectivity.root.facts_mut().scan_completion = Some(completion());
    let observed = estimate_concern(&selectivity);
    assert!(observed.detail.contains(&completed_counts(&selectivity)));
    assert!(observed.detail.contains(&t!("insight.relational_input.selectivity")));
    assert!(!observed.detail.contains(&t!("insight.relational_input.completed_input_volume")));
    assert!(observed.fix.starts_with(&t!("insight.relational_input.investigate_selectivity")));
}

#[test]
fn healthy_large_completed_input_and_parameter_representation_differences_do_not_prove_harmful_reuse() {
    let mut plan = access_plan(10_000.0, 10_000.0, 100_000.0, 100_000.0);
    plan.root.facts_mut().scan_completion = Some(completion());
    plan.facts_mut().parameter_context = Some(crate::plan_facts::PlanParameterContext {
        parameters: vec![crate::plan_facts::PlanParameter {
            name: Some("@filter".into()),
            data_type: Some("int".into()),
            compiled: crate::plan_facts::PlanParameterValueState::Recorded,
            runtime: crate::plan_facts::PlanParameterValueState::Recorded,
            comparison: crate::plan_facts::PlanParameterComparison::DifferentRepresentation,
        }],
        uses: vec![],
        issues: vec![],
    });
    let observed = insights(&plan);
    assert!(!observed.iter().any(|insight| matches!(insight.rule.as_str(), "misestimate" | "relational-input-access")));
    let parameter = observed.iter().find(|insight| insight.rule == "parameter-context").unwrap();
    assert_eq!(parameter.severity, Severity::Low);
    assert!(parameter.fix_sql.is_none());
}

#[test]
fn incomplete_repeated_threaded_unqualified_and_partial_access_never_claim_complete_input_cardinality() {
    for case in [
        "missing",
        "unfinished",
        "unknown-parallel",
        "parallel",
        "reexecuted",
        "rewound",
        "rebound",
        "worker",
        "unknown-input",
        "local-input",
        "seek",
        "index",
        "row-goal",
        "missing-access",
        "missing-output",
        "fractional-access",
        "estimated",
        "top",
        "filter",
        "join",
        "spool",
        "duplicate-node",
    ] {
        let mut plan = completed_input();
        match case {
            "missing" => plan.root.facts_mut().scan_completion = None,
            "unfinished" => {
                plan.root.facts_mut().scan_completion.as_mut().unwrap().threads[0].end_of_scans = Some("0".into())
            }
            "unknown-parallel" => plan.root.facts_mut().scan_completion.as_mut().unwrap().parallel = None,
            "parallel" => plan.root.facts_mut().scan_completion.as_mut().unwrap().parallel = Some(true),
            "reexecuted" => {
                plan.root.act_executions = Some(2.0);
                plan.root.facts_mut().scan_completion.as_mut().unwrap().threads[0].executions = Some("2".into());
            }
            "rewound" => plan.root.facts_mut().scan_completion.as_mut().unwrap().threads[0].rewinds = Some("1".into()),
            "rebound" => plan.root.facts_mut().scan_completion.as_mut().unwrap().threads[0].rebinds = Some("2".into()),
            "worker" => plan.root.facts_mut().scan_completion.as_mut().unwrap().threads[0].thread = 1,
            "unknown-input" => {
                plan.root.facts_mut().relational_input =
                    Some(RelationalInput { kind: Kind::Unknown, provenance: Provenance::Unresolved })
            }
            "local-input" => {
                plan.root.facts_mut().relational_input =
                    Some(RelationalInput { kind: Kind::LocalTableVariable, provenance: Provenance::NativeDeclaration })
            }
            "seek" => plan.root.kind = NodeKind::Seek,
            "index" => plan.root.index = Some("input-index".into()),
            "row-goal" => plan.root.facts_mut().estimated_rows_without_goal = Some(10_000.0),
            "missing-access" => plan.root.facts_mut().access_rows.as_mut().unwrap().actual_total = None,
            "missing-output" => plan.root.act_rows = None,
            "fractional-access" => plan.root.facts_mut().access_rows.as_mut().unwrap().actual_total = Some(10_000.5),
            "estimated" => plan.kind = PlanKind::Estimated,
            "top" | "filter" | "join" | "spool" | "duplicate-node" => {
                let (op, kind) = match case {
                    "top" => ("Top", NodeKind::Other),
                    "filter" => ("Filter", NodeKind::Other),
                    "join" => ("Nested Loops", NodeKind::Join),
                    "duplicate-node" => ("Stream Aggregate", NodeKind::Aggregate),
                    _ => ("Table Spool", NodeKind::Spool),
                };
                let mut parent = measured(0, op, kind, 1.0, 1.0, 1.0);
                if case == "duplicate-node" {
                    parent.id = plan.root.id;
                }
                parent.children.push(plan.root);
                plan.root = parent;
            }
            _ => unreachable!(),
        }
        // The negative fixture deliberately removes or corrupts counts; do not unwrap those facts to form
        // its expectation. These are the known original scan counts (two executions only in that case).
        let expected = t!(
            "insight.relational_input.completed_input_counts",
            estimated = if case == "reexecuted" { "200" } else { "100" },
            actual = "10K"
        );
        for concern in insights(&plan) {
            assert!(!concern.detail.contains(&expected), "{case}: {}", concern.detail);
        }
    }
}

#[test]
fn spill_and_type_conversion_remain_independent_competing_mechanisms_even_with_completed_input() {
    let mut scan = completed_input().root;
    scan.warnings.push(PlanWarning::ImplicitConversion);
    let expression = "CONVERT_IMPLICIT(nvarchar(20),[@Rows].[id],0)";
    scan.predicate = Some(format!("{expression}=N'10'"));
    let mut sort = measured(0, "Sort", NodeKind::Sort, 1000.0, 1000.0, 1.0);
    sort.facts_mut().serial_execution_count = Some("1".into());
    sort.warnings.push(PlanWarning::Spill);
    sort.runtime = Some(crate::NodeRuntime { spill_pages: Some(128.0), ..Default::default() });
    sort.children.push(scan);
    let mut plan = actual(Engine::Mssql, sort, 400.0);
    plan.facts_mut()
        .conversions
        .push(crate::PlanConversion { issue: crate::PlanConversionIssue::SeekPlan, expression: expression.into() });
    let observed = insights(&plan);
    let estimate = observed.iter().find(|insight| insight.rule == "misestimate").unwrap();
    assert!(estimate.detail.contains(&completed_counts(&plan)));
    assert!(estimate.detail.contains("does not establish why"));
    assert!(!estimate.detail.contains("as a result"));
    assert!(observed.iter().any(|insight| insight.rule == "spill"));
    assert!(observed.iter().any(|insight| insight.rule == "implicit-conversion"));
}

#[test]
fn every_aggregate_compute_or_sort_ancestor_requires_exact_source_issued_serial_execution_proof() {
    for kind in [NodeKind::Aggregate, NodeKind::Compute, NodeKind::Sort] {
        for missing in ["missing", "repeated", "noncanonical", "too-large", "parallel", "unknown-output"] {
            let mut parent = measured(0, "native ancestor", kind, 1.0, 1.0, 1.0);
            parent.facts_mut().serial_execution_count = Some("1".into());
            parent.children.push(completed_input().root);
            let mut plan = actual(Engine::Mssql, parent, 400.0);
            let expected = completed_counts(&plan);
            assert!(estimate_concern(&plan).detail.contains(&expected));
            match missing {
                "missing" => plan.root.facts_mut().serial_execution_count = None,
                "repeated" => plan.root.facts_mut().serial_execution_count = Some("2".into()),
                "noncanonical" => plan.root.facts_mut().serial_execution_count = Some("01".into()),
                "too-large" => plan.root.facts_mut().serial_execution_count = Some("9007199254740993".into()),
                "parallel" => plan.root.facts_mut().parallel_threads = Some(1),
                _ => plan.root.act_rows = None,
            }
            assert!(!estimate_concern(&plan).detail.contains(&expected), "{kind:?}: {missing}");
            assert!(estimate_concern(&plan).detail.contains(&t!(
                "insight.relational_input.access_counts",
                estimated = "100",
                actual = "10K"
            )));
        }
    }
}
