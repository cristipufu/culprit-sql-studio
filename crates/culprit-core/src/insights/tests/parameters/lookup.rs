use super::*;

fn lookup_plan() -> Plan {
    let mut plan = parameterized_seek();
    let mut seek = plan.root.clone();
    seek.id = 1;
    seek.facts_mut().parallel_operator = Some(false);
    seek.facts_mut().table_alias = Some("r".into());
    let mut lookup = measured(2, "Key Lookup", NodeKind::Lookup, 1.0, 10_000.0, 10_000.0);
    lookup.object = seek.object.clone();
    lookup.index = Some("pk_orders".into());
    lookup.seek_predicate = Some("orders.id = r.id".into());
    lookup.facts_mut().parallel_operator = Some(false);
    lookup.facts_mut().serial_execution_count = Some("10000".into());
    lookup.facts_mut().table_alias = Some("r".into());
    lookup.runtime = Some(crate::actual::NodeRuntime { logical_reads: Some(30_000.0), ..Default::default() });
    let mut join = measured(0, "Nested Loops", NodeKind::Join, 100.0, 10_000.0, 1.0);
    join.facts_mut().inner_join = Some(true);
    join.facts_mut().parallel_operator = Some(false);
    join.facts_mut().serial_execution_count = Some("1".into());
    join.children = vec![seek, lookup];
    plan.root = join;
    let context = plan.facts_mut().parameter_context.as_mut().unwrap();
    context.uses[0].node_id = 1;
    context.parameters[0].integer_values_equal = Some(false);
    context.retrieved_from_cache = Some(true);
    plan
}

fn lookup_insight(plan: &Plan) -> PlanInsight {
    insights(plan).into_iter().find(|insight| insight.rule == "key-lookups").expect("ordinary measured lookup work")
}

fn assert_ordinary(plan: &Plan, case: &str) {
    let mut ordinary = plan.clone();
    ordinary.facts_mut().parameter_context = None;
    let observed = insights(plan).into_iter().find(|insight| insight.rule == "key-lookups");
    let expected = insights(&ordinary).into_iter().find(|insight| insight.rule == "key-lookups");
    assert_eq!(
        serde_json::to_value(observed).unwrap(),
        serde_json::to_value(expected).unwrap(),
        "{case}: insufficient parameter attribution keeps the complete ordinary lookup explanation"
    );
}

#[test]
fn exact_cached_integer_seek_and_lookup_populations_enrich_the_existing_public_lookup_advice() {
    let plan = lookup_plan();
    let actual = lookup_insight(&plan);
    let mut ordinary = plan.clone();
    ordinary.facts_mut().parameter_context = None;
    let ordinary = lookup_insight(&ordinary);
    assert_eq!(
        (actual.title.as_str(), actual.severity, actual.impact),
        (ordinary.title.as_str(), ordinary.severity, ordinary.impact)
    );
    assert_eq!(actual.nodes, vec![2, 1, 0]);
    assert_eq!(
        actual.detail,
        format!(
            "{} {} {}",
            ordinary.detail,
            t!(
                "insight.parameter_lookup.detail",
                parameter = parameter_label_for_test(),
                estimated = "100",
                actual = "10K",
                node = "2",
                executions = "10K",
                reads = "30K"
            ),
            t!("insight.parameter_lookup.limit")
        )
    );
    assert_eq!(actual.fix, t!("insight.parameter_lookup.fix", covering = ordinary.fix));
    assert_eq!(actual.verify, t!("insight.parameter_lookup.verify"));
    assert!(actual.fix_sql.is_none());
    let context = context_insight(&plan).unwrap();
    assert_eq!((context.severity, context.impact), (Severity::Low, 0.0), "the native context is still informational");
}

#[test]
fn healthy_equal_unknown_or_unbound_parameter_context_keeps_the_ordinary_lookup_alternative() {
    for case in [
        "accurate",
        "same",
        "integer-equal",
        "integer-unknown",
        "not-cached",
        "cache-unknown",
        "use-missing",
        "use-wrong-node",
        "use-ambiguous",
        "unsupported-type",
        "estimated",
    ] {
        let mut plan = lookup_plan();
        match case {
            "accurate" => {
                plan.root.children[0].est_rows = 10_000.0;
                plan.root.children[0].facts_mut().access_rows.as_mut().unwrap().estimated_per_execution =
                    Some(10_000.0);
            }
            "same" => {
                let parameter = &mut plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0];
                parameter.comparison = Comparison::SameRepresentation;
                parameter.integer_values_equal = Some(true);
            }
            "integer-equal" => {
                plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0].integer_values_equal = Some(true)
            }
            "integer-unknown" => {
                plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0].integer_values_equal = None
            }
            "not-cached" => plan.facts_mut().parameter_context.as_mut().unwrap().retrieved_from_cache = Some(false),
            "cache-unknown" => plan.facts_mut().parameter_context.as_mut().unwrap().retrieved_from_cache = None,
            "use-missing" => plan.facts_mut().parameter_context.as_mut().unwrap().uses.clear(),
            "use-wrong-node" => plan.facts_mut().parameter_context.as_mut().unwrap().uses[0].node_id = 99,
            "use-ambiguous" => {
                plan.facts_mut().parameter_context.as_mut().unwrap().issues.push(PlanParameterIssue::AmbiguousUse)
            }
            "unsupported-type" => plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0].data_type = None,
            "estimated" => plan.kind = PlanKind::Estimated,
            _ => unreachable!(),
        }
        assert_ordinary(&plan, case);
    }
}

#[test]
fn unknown_parameter_facts_cannot_gain_lookup_attribution_from_any_engine_identity() {
    for engine in Engine::ALL {
        let mut plan = lookup_plan();
        plan.engine = engine;
        plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0].integer_values_equal = None;
        assert_ordinary(&plan, engine.spec().label());
        plan.facts_mut().parameter_context = None;
        assert_ordinary(&plan, engine.spec().label());
    }
}

#[test]
fn an_ancestor_needs_its_own_exact_serial_population_before_lookup_attribution() {
    let mut plan = lookup_plan();
    let mut parent = measured(3, "Stream Aggregate", NodeKind::Aggregate, 1.0, 1.0, 1.0);
    parent.facts_mut().parallel_operator = Some(false);
    parent.facts_mut().serial_execution_count = Some("1".into());
    parent.children.push(plan.root);
    plan.root = parent;
    assert_eq!(lookup_insight(&plan).verify, t!("insight.parameter_lookup.verify"));
    for case in ["missing", "repeated", "parallel", "row-goal"] {
        let mut changed = plan.clone();
        match case {
            "missing" => changed.root.facts_mut().serial_execution_count = None,
            "repeated" => {
                changed.root.act_executions = Some(2.0);
                changed.root.facts_mut().serial_execution_count = Some("2".into());
            }
            "parallel" => changed.root.facts_mut().parallel_operator = Some(true),
            "row-goal" => changed.root.facts_mut().estimated_rows_without_goal = Some(10.0),
            _ => unreachable!(),
        }
        assert_ordinary(&changed, case);
    }
}

#[test]
fn missing_parallel_or_contradictory_native_populations_cannot_lend_parameter_advice() {
    for case in [
        "seek-executions",
        "join-executions",
        "lookup-executions",
        "lookup-output",
        "join-output",
        "seek-access",
        "seek-output",
        "logical-reads-missing",
        "logical-reads-small",
        "logical-reads-fraction",
        "parallel",
        "planned-parallel",
        "planned-unknown",
        "row-goal",
    ] {
        let mut plan = lookup_plan();
        match case {
            "seek-executions" => plan.root.children[0].facts_mut().serial_execution_count = None,
            "join-executions" => plan.root.facts_mut().serial_execution_count = None,
            "lookup-executions" => plan.root.children[1].facts_mut().serial_execution_count = Some("9999".into()),
            "lookup-output" => plan.root.children[1].act_rows = Some(9999.0),
            "join-output" => plan.root.act_rows = Some(9999.0),
            "seek-access" => {
                plan.root.children[0].facts_mut().access_rows.as_mut().unwrap().actual_total = Some(20_000.0)
            }
            "seek-output" => plan.root.children[0].act_rows = None,
            "logical-reads-missing" => plan.root.children[1].runtime.as_mut().unwrap().logical_reads = None,
            "logical-reads-small" => plan.root.children[1].runtime.as_mut().unwrap().logical_reads = Some(1.0),
            "logical-reads-fraction" => plan.root.children[1].runtime.as_mut().unwrap().logical_reads = Some(30_000.5),
            "parallel" => plan.root.children[1].facts_mut().parallel_threads = Some(2),
            "planned-parallel" => plan.root.children[0].facts_mut().parallel_operator = Some(true),
            "planned-unknown" => plan.root.children[0].facts_mut().parallel_operator = None,
            "row-goal" => plan.root.facts_mut().estimated_rows_without_goal = Some(20_000.0),
            _ => unreachable!(),
        }
        assert_ordinary(&plan, case);
    }
}

#[test]
fn another_relation_alias_scope_or_duplicate_node_cannot_supply_the_lookup_relationship() {
    for case in [
        "different-object",
        "different-alias",
        "missing-alias",
        "residual",
        "missing-lookup-key",
        "hash-join",
        "outer-join",
        "duplicate-node",
        "another-input",
        "other-ancestor",
    ] {
        let mut plan = lookup_plan();
        match case {
            "different-object" => plan.root.children[1].object = Some("archive.orders".into()),
            "different-alias" => plan.root.children[1].facts_mut().table_alias = Some("other".into()),
            "missing-alias" => plan.root.children[0].facts_mut().table_alias = None,
            "residual" => plan.root.children[1].predicate = Some("orders.active = 1".into()),
            "missing-lookup-key" => plan.root.children[1].seek_predicate = None,
            "hash-join" => plan.root.op = "Hash Match".into(),
            "outer-join" => plan.root.facts_mut().inner_join = Some(false),
            "duplicate-node" => plan.root.children[1].id = 1,
            "another-input" => plan.root.children.push(measured(3, "Table Scan", NodeKind::Scan, 1.0, 1.0, 1.0)),
            "other-ancestor" => {
                let mut parent = measured(3, "Top", NodeKind::Other, 10_000.0, 10_000.0, 1.0);
                parent.facts_mut().parallel_operator = Some(false);
                parent.children.push(plan.root);
                plan.root = parent;
            }
            _ => unreachable!(),
        }
        assert_ordinary(&plan, case);
    }
}

#[test]
fn original_parameter_lookup_evidence_survives_redaction_and_a_saved_plan_roundtrip() {
    let mut plan = lookup_plan();
    plan.facts_mut().compiled_parameters = Some("@customer=9264".into());
    annotate(&mut plan);
    let original = lookup_insight(&plan);
    let redacted = crate::redact::redact_plan(&plan);
    let restored: Plan = serde_json::from_value(serde_json::to_value(&redacted).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(lookup_insight(&restored)).unwrap(), serde_json::to_value(&original).unwrap());
    assert_eq!(restored.root.children[1].runtime.as_ref().unwrap().logical_reads, Some(30_000.0));
    assert_eq!(restored.facts().parameter_context.as_ref().unwrap().parameters[0].integer_values_equal, Some(false));
    assert!(!serde_json::to_string(&restored).unwrap().contains("9264"));
    // A missing later actor does not replace this original plan. Its retained observation remains interpretable.
    assert_eq!(lookup_insight(&restored).nodes, vec![2, 1, 0]);
}
