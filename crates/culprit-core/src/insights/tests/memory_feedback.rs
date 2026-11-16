use super::*;
use crate::plan_facts::{
    PlanMemoryGrantFeedback, PlanMemoryGrantFeedbackIssue as Issue, PlanMemoryGrantFeedbackState as State,
};

fn disabled_plan() -> Plan {
    let mut plan = actual(Engine::Mssql, measured(0, "Sort", NodeKind::Sort, 100.0, 100.0, 1.0), 40.0);
    plan.facts_mut().memory_grant_feedback = Some(PlanMemoryGrantFeedback {
        state: State::DisabledUnstable,
        previous_grant_kb: Some("9007199254740993".into()),
        runtime_observed: true,
        issues: vec![],
    });
    plan
}

fn feedback_insight(plan: &Plan) -> Option<PlanInsight> {
    insights(plan).into_iter().find(|insight| insight.rule == MEMORY_GRANT_FEEDBACK_RULE)
}

#[test]
fn a_native_unstable_feedback_verdict_is_independent_low_scope_evidence_with_exact_prior_kilobytes() {
    let plan = disabled_plan();
    assert!(PlanMemoryGrantFeedback::disabled_in(&plan).is_some());
    let insight = feedback_insight(&plan).unwrap();
    assert_eq!(insight.severity, Severity::Low);
    assert_eq!(insight.impact, 0.0);
    assert!(insight.fixed_weight && insight.fix_sql.is_none());
    assert_eq!(insight.nodes, vec![plan.root.id]);
    assert!(insight.detail.contains(&format::digits("9007199254740993".into())));
    assert!(insight.detail.contains("prior execution"));
    assert!(!insight.detail.contains("9007199254740992"));
    assert!(plan.facts().memory_requested_kb.is_none());
    assert!(plan.facts().memory_granted_kb.is_none());
    assert!(plan.facts().compile_memory_kb.is_none());
    assert!(plan.facts().parameter_context.is_none());
}

#[test]
fn healthy_missing_unknown_and_ambiguous_feedback_states_do_not_become_new_faults() {
    for state in [
        State::NotRecorded,
        State::FirstExecution,
        State::AccurateGrant,
        State::Adjusting,
        State::PercentileAdjusting,
        State::Stable,
        State::Unrecognized,
    ] {
        let mut plan = disabled_plan();
        plan.facts_mut().memory_grant_feedback.as_mut().unwrap().state = state;
        assert!(feedback_insight(&plan).is_none(), "{state:?}");
        assert!(PlanMemoryGrantFeedback::disabled_in(&plan).is_none());
    }
    let mut plan = disabled_plan();
    plan.facts_mut().memory_grant_feedback = None;
    plan.props.insert("IsMemoryGrantFeedbackAdjusted".into(), "No: Feedback disabled".into());
    plan.facts_mut().server_version = Some("17.0".into());
    assert!(feedback_insight(&plan).is_none());
    plan.facts_mut().memory_grant_feedback = Some(PlanMemoryGrantFeedback {
        state: State::Unrecognized,
        previous_grant_kb: None,
        runtime_observed: false,
        issues: vec![Issue::AmbiguousMetadata],
    });
    assert!(feedback_insight(&plan).is_none());
}

#[test]
fn same_plan_actual_runtime_is_required_even_when_a_saved_field_claims_disabled_feedback() {
    for case in [
        "estimated",
        "generic",
        "no-rows",
        "no-executions",
        "zero-executions",
        "fractional-executions",
        "invalid-rows",
        "unqualified-native",
        "other-engine",
    ] {
        let mut plan = disabled_plan();
        match case {
            "estimated" => plan.kind = PlanKind::Estimated,
            "generic" => plan.kind = PlanKind::Generic,
            "no-rows" => plan.root.act_rows = None,
            "no-executions" => plan.root.act_executions = None,
            "zero-executions" => plan.root.act_executions = Some(0.0),
            "fractional-executions" => plan.root.act_executions = Some(1.5),
            "invalid-rows" => plan.root.act_rows = Some(f64::NAN),
            "unqualified-native" => plan.facts_mut().memory_grant_feedback.as_mut().unwrap().runtime_observed = false,
            "other-engine" => plan.engine = Engine::Postgres,
            _ => panic!(),
        }
        assert!(feedback_insight(&plan).is_none(), "{case}");
    }
}

#[test]
fn a_missing_or_malformed_prior_grant_never_becomes_zero_or_overrides_a_valid_native_state() {
    for issue in [None, Some(Issue::InvalidPreviousGrant)] {
        let mut plan = disabled_plan();
        let feedback = plan.facts_mut().memory_grant_feedback.as_mut().unwrap();
        feedback.previous_grant_kb = None;
        feedback.issues = issue.into_iter().collect();
        let insight = feedback_insight(&plan).unwrap();
        assert!(!insight.detail.contains("prior execution"));
        assert!(insight.fix_sql.is_none());
        assert_eq!(plan.facts().memory_granted_kb, None);
    }
    let mut plan = disabled_plan();
    plan.facts_mut().memory_grant_feedback.as_mut().unwrap().previous_grant_kb = Some("0".into());
    assert!(
        feedback_insight(&plan)
            .unwrap()
            .detail
            .contains(&t!("insight.memory_grant_feedback.previous_grant", kilobytes = "0"))
    );
}

#[test]
fn feedback_diagnosis_never_borrows_spill_impact_current_grants_or_parameter_causation() {
    let mut plan = disabled_plan();
    let without_spill = feedback_insight(&plan).unwrap();
    plan.root.warnings.push(PlanWarning::Spill);
    plan.root.runtime = Some(crate::actual::NodeRuntime { spill_pages: Some(640.0), ..Default::default() });
    plan.facts_mut().memory_requested_kb = Some(8192.0);
    plan.facts_mut().memory_granted_kb = Some(4096.0);
    plan.facts_mut().memory_max_used_kb = Some(4096.0);
    plan.facts_mut().compile_memory_kb = Some(2_000_000.0);
    let with_spill = feedback_insight(&plan).unwrap();
    assert_eq!(serde_json::to_value(&without_spill).unwrap(), serde_json::to_value(&with_spill).unwrap());
    assert!(insights(&plan).iter().any(|insight| insight.rule == "spill"));
    assert!(with_spill.detail.contains("does not identify"));
    assert!(with_spill.detail.contains("harmful plan reuse"));
    assert!(with_spill.fix.contains("does not justify"));
    assert!(with_spill.verify.contains("naturally occurring comparable executions"));
    assert!(with_spill.fix_sql.is_none());
}

#[test]
fn shared_plan_privacy_keeps_only_the_safe_native_verdict_and_exact_measurement() {
    let mut plan = disabled_plan();
    plan.raw = Some("private-native-host-query-canary".into());
    plan.statement = Some("SELECT N'private-native-constant-canary'".into());
    let shared = crate::redact::redact_plan(&plan);
    assert!(shared.raw.is_none());
    assert_eq!(shared.facts().memory_grant_feedback, plan.facts().memory_grant_feedback);
    assert!(PlanMemoryGrantFeedback::disabled_in(&shared).is_some());
    let encoded = serde_json::to_string(&feedback_insight(&shared).unwrap()).unwrap();
    assert!(!encoded.contains("private-native"));
    assert_eq!(
        serde_json::to_value(crate::redact::redact_plan(&shared)).unwrap(),
        serde_json::to_value(shared).unwrap()
    );
}
