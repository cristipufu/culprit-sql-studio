use super::*;

fn serial(node: &mut PlanNode) {
    node.facts_mut().serial_execution_count = Some("1".into());
    node.facts_mut().parallel_operator = Some(false);
}

fn hash_plan() -> Plan {
    let mut input = completed_input().root;
    serial(&mut input);
    let mut probe = measured(2, "Table Scan", NodeKind::Scan, 20_000.0, 20_000.0, 1.0);
    probe.object = Some("dbo.Items".into());
    serial(&mut probe);
    let mut hash = measured(3, "Hash Match", NodeKind::Join, 1000.0, 1000.0, 1.0);
    serial(&mut hash);
    hash.facts_mut().inner_join = Some(true);
    hash.facts_mut().hash_build_input = Some(input.id);
    hash.children = vec![input, probe];
    let mut root = measured(0, "Stream Aggregate", NodeKind::Aggregate, 1.0, 1.0, 1.0);
    serial(&mut root);
    root.children.push(hash);
    actual(Engine::Mssql, root, 400.0)
}

#[test]
fn serial_native_hash_build_preserves_complete_input_filtered_rows_and_final_output_separately() {
    let plan = hash_plan();
    let concern = estimate_concern(&plan);
    assert_eq!(concern.nodes, vec![1]);
    assert!(concern.detail.contains(&completed_counts(&plan)));
    assert_eq!(plan.root.children[0].children[0].act_rows, Some(1000.0));
    assert_eq!(
        concern.title,
        t!(
            "insight.misestimate.title_more",
            n = 100,
            count = "100",
            table = "@input",
            got = rows_text(1000.0),
            factor = "10"
        )
    );
    assert!(concern.detail.contains(&t!("insight.relational_input.root_output", actual = "1")));
    assert!(concern.detail.contains(&t!("insight.relational_input.access_and_selectivity")));
    assert!(concern.fix_sql.is_none());
    assert!(concern.fix.ends_with(&t!("insight.misestimate.fix.tvp", table = "@input")));
    let restored: Plan = serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
    assert_eq!(restored.root.children[0].facts().hash_build_input, Some(1));
    assert_eq!(estimate_concern(&restored).detail, concern.detail);
}

#[test]
fn native_probe_ownership_and_unknown_hash_role_do_not_turn_eof_into_complete_tvp_input() {
    for case in ["probe", "missing", "foreign", "display-only"] {
        let mut plan = hash_plan();
        let hash = &mut plan.root.children[0];
        match case {
            "probe" => hash.facts_mut().hash_build_input = Some(2),
            "missing" => hash.facts_mut().hash_build_input = None,
            "foreign" => hash.facts_mut().hash_build_input = Some(99),
            "display-only" => {
                hash.facts_mut().hash_build_input = None;
                hash.props.insert("HashBuildInput".into(), "1".into());
            }
            _ => unreachable!(),
        }
        let concern = estimate_concern(&plan);
        assert!(!concern.detail.contains(&completed_counts(&plan)), "{case}");
        assert!(concern.detail.contains(&t!(
            "insight.relational_input.access_counts",
            estimated = "100",
            actual = "10K"
        )));
        assert_eq!(concern.nodes, vec![1]);
        assert!(concern.fix_sql.is_none());
    }
}

#[test]
fn hash_build_requires_exact_serial_inner_join_endpoints_and_unique_native_node_identity() {
    for case in [
        "non-inner",
        "unknown-join",
        "parallel",
        "unknown-parallel",
        "missing-execution",
        "repeated",
        "row-goal",
        "parallel-probe",
        "unknown-probe",
        "missing-probe-execution",
        "repeated-probe",
        "unknown-probe-output",
        "probe-row-goal",
        "extra-input",
        "missing-eof",
        "rebound-input",
        "unproven-input",
    ] {
        let mut plan = hash_plan();
        let hash = &mut plan.root.children[0];
        match case {
            "non-inner" => hash.facts_mut().inner_join = Some(false),
            "unknown-join" => hash.facts_mut().inner_join = None,
            "parallel" => hash.facts_mut().parallel_operator = Some(true),
            "unknown-parallel" => hash.facts_mut().parallel_operator = None,
            "missing-execution" => hash.facts_mut().serial_execution_count = None,
            "repeated" => {
                hash.facts_mut().serial_execution_count = Some("2".into());
                hash.act_executions = Some(2.0);
            }
            "row-goal" => hash.facts_mut().estimated_rows_without_goal = Some(1000.0),
            "parallel-probe" => hash.children[1].facts_mut().parallel_operator = Some(true),
            "unknown-probe" => hash.children[1].facts_mut().parallel_operator = None,
            "missing-probe-execution" => hash.children[1].facts_mut().serial_execution_count = None,
            "repeated-probe" => {
                hash.children[1].facts_mut().serial_execution_count = Some("2".into());
                hash.children[1].act_executions = Some(2.0);
            }
            "unknown-probe-output" => hash.children[1].act_rows = None,
            "probe-row-goal" => hash.children[1].facts_mut().estimated_rows_without_goal = Some(20_000.0),
            "extra-input" => hash.children.push(measured(9, "Table Scan", NodeKind::Scan, 1.0, 1.0, 1.0)),
            "missing-eof" => hash.children[0].facts_mut().scan_completion = None,
            "rebound-input" => {
                hash.children[0].facts_mut().scan_completion.as_mut().unwrap().threads[0].rebinds = Some("2".into());
            }
            "unproven-input" => hash.children[0].facts_mut().relational_input = None,
            _ => unreachable!(),
        }
        let concern = estimate_concern(&plan);
        assert!(!concern.detail.contains(&completed_counts(&plan)), "{case}: {}", concern.detail);
        assert!(concern.fix_sql.is_none(), "{case}: {:?}", concern.fix_sql);
    }
}

#[test]
fn duplicate_node_ids_cannot_transfer_a_tvp_mismatch_to_accurate_named_counters_or_fix_sql() {
    for reverse in [false, true] {
        let mut plan = hash_plan();
        let hash = &mut plan.root.children[0];
        hash.children[1].id = hash.children[0].id;
        if reverse {
            hash.children.reverse();
        }
        let raw_counts = |plan: &Plan| {
            plan.nodes()
                .into_iter()
                .map(|node| (node.object.clone(), node.act_rows, node.facts().access_rows))
                .collect::<Vec<_>>()
        };
        let original = raw_counts(&plan);
        assert!(!plan.root.has_unique_node_ids());
        assert!(plan.root.misestimates().is_empty());
        plan.root.flag_misestimates();
        assert!(plan.nodes().iter().all(|node| !node.warnings.contains(&PlanWarning::Misestimate)));
        let observed = insights(&plan);
        assert!(
            observed.iter().all(|insight| !matches!(insight.rule.as_str(), "misestimate" | "relational-input-access"))
        );
        assert!(observed.iter().all(|insight| insight.fix_sql.is_none()));
        assert_eq!(raw_counts(&plan), original);
        assert_eq!(
            plan.root.children[0]
                .children
                .iter()
                .find(|node| node.object.as_deref() == Some("dbo.Items"))
                .unwrap()
                .misestimate(),
            Some(1.0)
        );
    }
}

#[test]
fn early_stop_or_unknown_ancestors_still_refuse_a_complete_input_claim_above_a_known_hash_build() {
    for kind in [NodeKind::Other, NodeKind::Spool, NodeKind::Join] {
        let mut plan = hash_plan();
        plan.root.kind = kind;
        let concern = estimate_concern(&plan);
        assert!(!concern.detail.contains(&completed_counts(&plan)), "{kind:?}");
    }
    let mut plan = hash_plan();
    plan.root.facts_mut().parallel_operator = Some(true);
    assert!(!estimate_concern(&plan).detail.contains(&completed_counts(&plan)));
}

#[test]
fn accurate_large_hash_build_and_independent_spill_evidence_do_not_create_harmful_reuse() {
    let mut plan = hash_plan();
    let input = &mut plan.root.children[0].children[0];
    input.est_rows = 1000.0;
    input.facts_mut().access_rows.as_mut().unwrap().estimated_per_execution = Some(10_000.0);
    let observed = insights(&plan);
    assert!(observed.iter().all(|insight| !matches!(insight.rule.as_str(), "misestimate" | "relational-input-access")));

    let mut plan = hash_plan();
    let hash = &mut plan.root.children[0];
    hash.warnings.push(PlanWarning::Spill);
    hash.runtime = Some(crate::NodeRuntime { spill_pages: Some(128.0), ..Default::default() });
    let observed = insights(&plan);
    assert!(observed.iter().any(|insight| insight.rule == "spill"));
    let concern = observed.iter().find(|insight| insight.rule == "misestimate").unwrap();
    assert!(concern.detail.contains(&completed_counts(&plan)));
    assert!(concern.detail.contains("does not establish why"));
    assert!(concern.fix_sql.is_none());
}
