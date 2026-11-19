use super::*;
use crate::plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance};

fn spilling_plan(kind: NodeKind, op: &str) -> Plan {
    let mut operator = measured(1, op, kind, 10_000.0, 10_000.0, 1.0);
    operator.warnings.push(PlanWarning::Spill);
    operator.runtime = Some(crate::actual::NodeRuntime { spill_pages: Some(640.0), ..Default::default() });
    actual(Engine::Mssql, operator, 500.0)
}

fn spill_insight(plan: &Plan) -> PlanInsight {
    insights(plan).into_iter().find(|insight| insight.rule == "spill").expect("native spill retained")
}

#[test]
fn accurate_or_unknown_estimates_preserve_the_native_spill_without_a_cause() {
    for actual_rows in [Some(10_000.0), None] {
        let mut plan = spilling_plan(NodeKind::Sort, "Sort");
        plan.root.act_rows = actual_rows;
        let insight = spill_insight(&plan);
        assert_eq!(insight.nodes, vec![1]);
        assert!(insight.title.contains("5.0 MB"));
        assert!(insight.detail.contains("does not establish its elapsed cost or underlying cause"));
        assert!(!insight.detail.contains("far more rows"));
        assert!(insight.fix.contains("applicable limits and concurrent demand"));
        assert!(!insight.fix.contains("statistics"));
        assert!(insight.fix_sql.is_none());
        assert_eq!(crate::plan_facts::spill_pages(&plan.root), Some(640.0));
    }
}

#[test]
fn a_recorded_underestimate_is_co_evidence_without_a_stale_statistics_verdict() {
    let mut plan = spilling_plan(NodeKind::Join, "Hash Match");
    let mut input = measured(2, "Table Scan", NodeKind::Scan, 100.0, 10_000.0, 1.0);
    input.object = Some("dbo.input_rows".into());
    plan.root.children.push(input);
    let insight = spill_insight(&plan);
    assert!(insight.detail.contains("returned far more rows than estimated"));
    assert!(insight.detail.contains("mismatch alone does not establish"));
    assert!(!insight.fix.contains("UPDATE STATISTICS"));
    assert!(!insight.fix.contains("Fix the row estimate first"));
    assert_eq!(plan.root.children[0].act_rows, Some(10_000.0));
    assert_eq!(plan.root.children[0].est_rows, 100.0);
}

#[test]
fn native_relational_inputs_never_receive_automatic_statistics_or_compatibility_commands() {
    for input in [
        RelationalInput {
            kind: RelationalInputKind::TableValuedParameter,
            provenance: RelationalInputProvenance::NativeParameterMetadata,
        },
        RelationalInput {
            kind: RelationalInputKind::LocalTableVariable,
            provenance: RelationalInputProvenance::NativeDeclaration,
        },
        RelationalInput { kind: RelationalInputKind::Unknown, provenance: RelationalInputProvenance::Unresolved },
    ] {
        let mut plan = spilling_plan(NodeKind::Join, "Hash Match");
        let mut rows = measured(2, "Table Scan", NodeKind::Scan, 100.0, 10_000.0, 1.0);
        rows.object = Some("@input".into());
        rows.facts_mut().relational_input = Some(input);
        plan.root.children.push(rows);
        let insight = spill_insight(&plan);
        assert!(insight.detail.contains("returned far more rows than estimated"));
        assert!(insight.fix_sql.is_none());
        assert!(!insight.fix.contains("statistics"));
        assert!(!insight.fix.contains("compatibility"));
        assert_eq!(plan.root.children[0].facts().relational_input, Some(input));
    }
}

#[test]
fn only_a_native_sort_kind_can_suggest_testing_ordered_access() {
    for (kind, op, ordered_access) in [
        (NodeKind::Sort, "Sort", true),
        (NodeKind::Join, "Hash Match", false),
        (NodeKind::Aggregate, "Hash Match", false),
        (NodeKind::Other, "Sort", false),
    ] {
        let insight = spill_insight(&spilling_plan(kind, op));
        assert_eq!(insight.fix.contains("supply the required order"), ordered_access);
        assert!(insight.fix_sql.is_none());
        assert!(!insight.fix.contains("removes the sort"));
    }
}

#[test]
fn version_or_display_properties_do_not_authorize_memory_feedback_or_a_grant_change() {
    for version in [None, Some("14.0"), Some("15.0"), Some("16.0")] {
        let mut plan = spilling_plan(NodeKind::Join, "Hash Match");
        plan.statement = Some("SELECT * FROM dbo.items OPTION (RECOMPILE)".into());
        plan.facts_mut().server_version = version.map(str::to_string);
        plan.props.insert("IsMemoryGrantFeedbackAdjusted".into(), "No: Feedback disabled".into());
        plan.facts_mut().memory_requested_kb = Some(8192.0);
        plan.facts_mut().memory_granted_kb = Some(4096.0);
        plan.facts_mut().memory_max_used_kb = Some(4096.0);
        let insight = spill_insight(&plan);
        assert!(insight.fix.contains("check native memory grant feedback eligibility and status"));
        assert!(!insight.fix.contains("2019") && !insight.fix.contains("150"));
        assert!(insight.fix_sql.is_none());
        assert_eq!(plan.facts().memory_requested_kb, Some(8192.0));
        assert_eq!(plan.facts().memory_granted_kb, Some(4096.0));
        assert_eq!(plan.facts().memory_max_used_kb, Some(4096.0));
    }
}

#[test]
fn verification_compares_equivalent_work_and_does_not_demand_zero_spills() {
    let insight = spill_insight(&spilling_plan(NodeKind::Sort, "Sort"));
    assert!(insight.verify.contains("equivalent load"));
    assert!(insight.verify.contains("elapsed time, CPU and grant waiting"));
    assert!(insight.verify.contains("Fewer spills alone do not establish a better result"));
    assert!(insight.verify.contains("increase concurrency pressure"));
    assert!(!insight.verify.contains("no spill"));
}

#[test]
fn postgres_hash_batches_keep_native_evidence_without_an_estimate_sized_grant_or_sort_cure() {
    for engine in [Engine::Postgres, Engine::Yugabyte] {
        let mut plan = spilling_plan(NodeKind::Join, "Hash");
        plan.engine = engine;
        plan.root.runtime = None;
        plan.root.facts_mut().hash_batches = Some(4);
        let insight = spill_insight(&plan);
        assert!(insight.title.contains("4 batches"));
        assert!(insight.fix.contains("work_mem") && insight.fix.contains("hash_mem_multiplier"));
        assert!(!insight.fix.contains("required order"));
        assert!(insight.fix_sql.is_none());
        assert!(insight.detail.contains("does not establish elapsed cost"));
        assert!(insight.verify.contains("equivalent load"));
        assert!(!insight.verify.contains("Hash Batches is 1"));
        assert_eq!(plan.root.facts().hash_batches, Some(4));
    }
}

#[test]
fn native_mysql_merge_passes_are_recorded_without_sizing_session_memory_or_requiring_zero_passes() {
    let mut plan = spilling_plan(NodeKind::Sort, "Sort");
    plan.engine = Engine::Mysql;
    plan.facts_mut().flavor = Some("MariaDB".into());
    plan.root.runtime = None;
    plan.root.facts_mut().sort_merge_passes = Some(3);
    let insight = spill_insight(&plan);
    assert!(insight.title.contains("3 merge passes"));
    assert!(insight.fix.contains("sort_buffer_size"));
    assert!(insight.fix.contains("supply the required order"));
    assert!(insight.fix_sql.is_none());
    assert!(insight.verify.contains("a zero pass count is not required"));
    assert_eq!(plan.root.facts().sort_merge_passes, Some(3));
}

#[test]
fn an_unquantified_mysql_spill_never_invents_one_merge_pass_or_a_sort_only_remedy() {
    let mut plan = spilling_plan(NodeKind::Other, "Materialize");
    plan.engine = Engine::Mysql;
    plan.root.runtime = None;
    let insight = spill_insight(&plan);
    assert!(insight.title.contains("reported a spill"));
    assert!(!insight.title.contains("1 merge pass"));
    assert!(insight.detail.contains("without a recorded merge-pass count"));
    assert!(insight.fix.contains("filesort or an internal temporary table"));
    assert!(!insight.fix.contains("required order"));
    assert!(insight.fix_sql.is_none());
}

#[test]
fn actual_spills_distinguish_statement_grants_below_equal_and_above_the_request() {
    for (granted, explanation) in [
        (4096.0, t!("insight.spill.tempdb.grant.below", requested = "8.0 MB", granted = "4.0 MB")),
        (8192.0, t!("insight.spill.tempdb.grant.equal", requested = "8.0 MB", granted = "8.0 MB")),
        (12288.0, t!("insight.spill.tempdb.grant.above", requested = "8.0 MB", granted = "12.0 MB")),
        (0.0, t!("insight.spill.tempdb.grant.below", requested = "8.0 MB", granted = "0 B")),
    ] {
        let mut plan = spilling_plan(NodeKind::Sort, "Sort");
        let mut expected = spill_insight(&plan);
        plan.facts_mut().memory_requested_kb = Some(8192.0);
        plan.facts_mut().memory_granted_kb = Some(granted);
        expected.detail = format!("{} {explanation} {}", expected.detail, t!("insight.spill.tempdb.grant.scope"));
        assert_eq!(spill_insight(&plan), expected, "grant {granted} changes only the same-plan explanation");
        assert_eq!(plan.facts().memory_requested_kb, Some(8192.0));
        assert_eq!(plan.facts().memory_granted_kb, Some(granted));
    }
}

#[test]
fn missing_invalid_or_unrepresentable_statement_grants_preserve_the_exact_generic_spill() {
    let expected = spill_insight(&spilling_plan(NodeKind::Sort, "Sort"));
    for (requested, granted) in [
        (None, None),
        (None, Some(4096.0)),
        (Some(8192.0), None),
        (Some(0.0), Some(0.0)),
        (Some(0.0), Some(4096.0)),
        (Some(-1.0), Some(4096.0)),
        (Some(8192.0), Some(-1.0)),
        (Some(f64::NAN), Some(4096.0)),
        (Some(8192.0), Some(f64::NAN)),
        (Some(f64::INFINITY), Some(4096.0)),
        (Some(8192.0), Some(f64::INFINITY)),
        (Some(f64::MAX), Some(4096.0)),
        (Some(8192.0), Some(f64::MAX)),
    ] {
        let mut plan = spilling_plan(NodeKind::Sort, "Sort");
        plan.facts_mut().memory_requested_kb = requested;
        plan.facts_mut().memory_granted_kb = granted;
        plan.props.insert("RequestedMemory".into(), "8192".into());
        plan.props.insert("GrantedMemory".into(), "4096".into());
        assert_eq!(
            spill_insight(&plan),
            expected,
            "native values {requested:?}/{granted:?}; properties cannot fill gaps"
        );
    }
}

#[test]
fn estimated_or_generic_plans_cannot_lend_actual_statement_grants_even_with_measured_children() {
    for kind in [PlanKind::Estimated, PlanKind::Generic] {
        let mut plan = spilling_plan(NodeKind::Sort, "Sort");
        plan.kind = kind;
        assert!(plan.root.act_rows.is_some());
        let expected = spill_insight(&plan);
        plan.facts_mut().memory_requested_kb = Some(8192.0);
        plan.facts_mut().memory_granted_kb = Some(4096.0);
        assert_eq!(spill_insight(&plan), expected, "{kind:?}");
    }
}

#[test]
fn other_engines_do_not_borrow_sql_server_statement_grants_for_their_spills() {
    for engine in Engine::ALL.iter().copied().filter(|engine| *engine != Engine::Mssql) {
        let mut plan = spilling_plan(NodeKind::Sort, "Sort");
        plan.engine = engine;
        let expected = insights(&plan).into_iter().filter(|insight| insight.rule == "spill").collect::<Vec<_>>();
        plan.facts_mut().memory_requested_kb = Some(8192.0);
        plan.facts_mut().memory_granted_kb = Some(4096.0);
        let actual = insights(&plan).into_iter().filter(|insight| insight.rule == "spill").collect::<Vec<_>>();
        assert_eq!(actual, expected, "{engine:?}");
    }
}

#[test]
fn a_statement_grant_relation_without_native_spill_evidence_creates_no_spill() {
    let mut plan = spilling_plan(NodeKind::Sort, "Sort");
    plan.root.warnings.clear();
    plan.root.runtime = None;
    for granted in [4096.0, 8192.0, 12288.0] {
        plan.facts_mut().memory_requested_kb = Some(8192.0);
        plan.facts_mut().memory_granted_kb = Some(granted);
        assert!(!insights(&plan).iter().any(|insight| insight.rule == "spill"));
    }
}

#[test]
fn same_plan_grant_delivery_does_not_move_an_unrelated_estimate_mismatch_under_the_spill() {
    for related in [false, true] {
        let mut sort = spilling_plan(NodeKind::Sort, "Sort").root;
        let input = measured(2, "Table Scan", NodeKind::Scan, 100.0, 10_000.0, 1.0);
        let mut root = measured(0, "Nested Loops", NodeKind::Join, 10_000.0, 10_000.0, 1.0);
        if related {
            sort.children.push(input);
            root.children.push(sort);
        } else {
            root.children = vec![sort, input];
        }
        let mut plan = actual(Engine::Mssql, root, 500.0);
        plan.facts_mut().memory_requested_kb = Some(8192.0);
        plan.facts_mut().memory_granted_kb = Some(4096.0);
        let insight = spill_insight(&plan);
        assert_eq!(insight.nodes, [1]);
        assert!(insight.detail.contains(&t!(
            "insight.spill.tempdb.grant.below",
            requested = "8.0 MB",
            granted = "4.0 MB"
        )));
        assert_eq!(insight.detail.contains(&t!("insight.spill.tempdb.underestimated")), related);
        assert_eq!(insight.fix, format!("{} {}", t!("insight.spill.tempdb.fix"), t!("insight.spill.fix.order")));
        assert!(insight.fix_sql.is_none());
    }
}

#[test]
fn multiple_spilling_operators_keep_the_whole_statement_grant_separate_from_each_allocation() {
    let sort = spilling_plan(NodeKind::Sort, "Sort").root;
    let mut hash = sort.clone();
    hash.id = 2;
    hash.op = "Hash Match".into();
    hash.kind = NodeKind::Join;
    hash.runtime.as_mut().unwrap().spill_pages = Some(320.0);
    let mut root = measured(0, "Nested Loops", NodeKind::Join, 10_000.0, 10_000.0, 1.0);
    root.children = vec![sort, hash];
    let mut plan = actual(Engine::Mssql, root, 500.0);
    plan.facts_mut().memory_requested_kb = Some(8192.0);
    plan.facts_mut().memory_granted_kb = Some(4096.0);
    let spills = insights(&plan).into_iter().filter(|insight| insight.rule == "spill").collect::<Vec<_>>();
    assert_eq!(spills.len(), 2);
    for insight in spills {
        assert!(insight.detail.contains(&t!(
            "insight.spill.tempdb.grant.below",
            requested = "8.0 MB",
            granted = "4.0 MB"
        )));
        assert!(insight.detail.contains(&t!("insight.spill.tempdb.grant.scope")));
        assert_eq!(insight.nodes.len(), 1);
        assert!(insight.fix_sql.is_none());
    }
    assert_eq!(plan.facts().memory_granted_kb, Some(4096.0));
    assert_eq!(crate::plan_facts::spill_pages(&plan.root.children[0]), Some(640.0));
    assert_eq!(crate::plan_facts::spill_pages(&plan.root.children[1]), Some(320.0));
}

#[test]
fn native_exchange_spills_do_not_acquire_a_workspace_memory_explanation() {
    let mut plan = spilling_plan(NodeKind::Other, "Parallelism");
    plan.root.warnings.clear();
    plan.root.facts_mut().exchange_spill = Some(true);
    let expected = spill_insight(&plan);
    plan.facts_mut().memory_requested_kb = Some(8192.0);
    plan.facts_mut().memory_granted_kb = Some(4096.0);
    assert_eq!(spill_insight(&plan), expected);
}
