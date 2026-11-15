use super::*;

fn native_limit(reason: Option<&str>) -> Plan {
    let mut plan = plan(Engine::Mssql, node(0, "Table Scan", NodeKind::Scan));
    plan.facts_mut().optimization_early_abort = reason.map(str::to_owned);
    plan
}

fn limit(plan: &Plan) -> Option<PlanInsight> {
    insights(plan).into_iter().find(|insight| insight.rule == "optimizer-timeout")
}

#[test]
fn a_native_optimizer_memory_limit_identifies_compilation_constraint_without_current_ownership() {
    let mut plan = native_limit(Some("MemoryLimitExceeded"));
    plan.facts_mut().compile_ms = Some(800.0);
    plan.facts_mut().compile_cpu_ms = Some(700.0);
    plan.facts_mut().compile_memory_kb = Some(32768.0);
    let insight = limit(&plan).unwrap();
    assert_eq!(insight.title, t!("insight.optimizer_timeout.title_memory"));
    assert!(insight.detail.contains("MemoryLimitExceeded"));
    assert!(insight.detail.contains("does not identify a current compiler memory owner"));
    for context in [
        t!("insight.optimizer_timeout.context.elapsed", elapsed = "800 ms"),
        t!("insight.optimizer_timeout.context.cpu", cpu = "700 ms"),
        t!("insight.optimizer_timeout.context.memory", memory = "32768 KiB"),
    ] {
        assert!(insight.detail.contains(&context), "{}", insight.detail);
    }
    assert_eq!(insight.severity, Severity::Low);
    assert!(insight.fixed_weight);
    assert!(insight.fix_sql.is_none());
    assert!(insight.fix.contains("memory settings"));
    assert!(insight.verify.contains("combined"));
}

#[test]
fn native_search_budget_exhaustion_is_not_a_wall_clock_or_execution_timeout() {
    let plan = native_limit(Some("TimeOut"));
    let insight = limit(&plan).unwrap();
    assert_eq!(insight.title, t!("insight.optimizer_timeout.title"));
    assert_eq!(insight.detail, t!("insight.optimizer_timeout.detail.search_budget"));
    assert!(insight.detail.contains("not a wall-clock or execution timeout"));
    assert!(insight.fix.contains("cardinality estimates"));
    assert!(!insight.title.contains("best plan"));
    assert!(insight.fix_sql.is_none());
}

#[test]
fn normal_early_stop_unknown_labels_and_display_properties_do_not_manufacture_a_limit_diagnosis() {
    for reason in [None, Some("GoodEnoughPlanFound"), Some("unknown-native-reason"), Some("timeout")] {
        let mut plan = native_limit(reason);
        plan.props.insert("StatementOptmEarlyAbortReason".into(), "MemoryLimitExceeded".into());
        plan.facts_mut().compile_memory_kb = Some(1_000_000.0);
        assert!(limit(&plan).is_none(), "{reason:?}");
    }
    for engine in Engine::ALL.into_iter().filter(|engine| *engine != Engine::Mssql) {
        let mut plan = native_limit(Some("TimeOut"));
        plan.engine = engine;
        assert!(limit(&plan).is_none(), "{engine:?} cannot inherit SQL Server native reason semantics");
    }
}

#[test]
fn invalid_or_missing_footprints_stay_unknown_and_zero_measurements_remain_measured_zero() {
    for value in [None, Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
        let mut plan = native_limit(Some("MemoryLimitExceeded"));
        let facts = plan.facts_mut();
        facts.compile_ms = value;
        facts.compile_cpu_ms = value;
        facts.compile_memory_kb = value;
        assert_eq!(limit(&plan).unwrap().detail, t!("insight.optimizer_timeout.detail.compiler_memory"));
    }
    let mut plan = native_limit(Some("MemoryLimitExceeded"));
    let facts = plan.facts_mut();
    facts.compile_ms = Some(0.0);
    facts.compile_cpu_ms = Some(0.0);
    facts.compile_memory_kb = Some(0.0);
    let insight = limit(&plan).unwrap();
    assert!(insight.detail.contains(&t!("insight.optimizer_timeout.context.memory", memory = "0 KiB")));
    assert!(insight.detail.contains(&t!("insight.optimizer_timeout.context.elapsed", elapsed = format::duration(0.0))));
}

#[test]
fn execution_grants_and_other_statements_memory_never_become_this_compilation_footprint() {
    let mut plan = native_limit(Some("MemoryLimitExceeded"));
    plan.facts_mut().memory_desired_kb = Some(2_000_000.0);
    plan.facts_mut().memory_granted_kb = Some(1_000_000.0);
    plan.facts_mut().memory_max_used_kb = Some(800_000.0);
    let mut child = node(1, "Index Scan", NodeKind::Scan);
    child.props.insert("CompileMemory".into(), "999999".into());
    plan.root.children.push(child);
    let insight = limit(&plan).unwrap();
    assert_eq!(insight.detail, t!("insight.optimizer_timeout.detail.compiler_memory"));
    assert!(!insight.detail.contains("999999"));
}

#[test]
fn even_a_slow_measured_execution_does_not_prove_that_the_optimization_limit_caused_runtime_harm() {
    let mut plan = native_limit(Some("TimeOut"));
    plan.kind = PlanKind::Actual;
    plan.facts_mut().elapsed_ms = Some(60_000.0);
    plan.root.act_rows = Some(1_000_000.0);
    plan.root.act_executions = Some(1.0);
    let insight = limit(&plan).unwrap();
    assert_eq!(insight.severity, Severity::Low);
    assert!(insight.fixed_weight);
    assert!(insight.fix_sql.is_none());
    assert!(insight.verify.contains("reason alone"));
}
