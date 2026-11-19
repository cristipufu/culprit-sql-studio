use super::*;
use crate::plan_facts::{
    PlanParameter, PlanParameterComparison as Comparison, PlanParameterContext, PlanParameterIssue,
    PlanParameterValueState as State,
};

mod lookup;

fn parameter(name: &str) -> PlanParameter {
    PlanParameter {
        integer_values_equal: None,
        name: Some(name.into()),
        data_type: Some("int".into()),
        compiled: State::Recorded,
        runtime: State::Recorded,
        comparison: Comparison::DifferentRepresentation,
    }
}

fn recorded_plan() -> Plan {
    let mut plan = actual(Engine::Mssql, measured(0, "Table Scan", NodeKind::Scan, 1.0, 1.0, 1.0), 400.0);
    plan.facts_mut().parameter_context = Some(PlanParameterContext {
        retrieved_from_cache: None,
        parameters: vec![parameter("@customer")],
        uses: vec![],
        issues: vec![],
    });
    plan
}

fn context_insight(plan: &Plan) -> Option<PlanInsight> {
    insights(plan).into_iter().find(|insight| insight.rule == "parameter-context")
}

#[test]
fn native_parameter_comparison_is_low_scope_context_without_a_reuse_or_mutation_verdict() {
    let mut plan = recorded_plan();
    plan.facts_mut().compiled_parameters = Some("@customer = 'private-compiled-canary'".into());
    let insight = context_insight(&plan).expect("a recorded native representation difference");
    assert_eq!(insight.severity, Severity::Low);
    assert!(insight.fixed_weight);
    assert_eq!(insight.impact, 0.0);
    assert!(insight.fix_sql.is_none());
    assert!(insight.detail.contains(&t!(
        "insight.parameter_context.named_type",
        parameter = "@customer",
        data_type = "int"
    )));
    assert!(insight.detail.contains("not SQL value equivalence"));
    assert!(insight.fix.contains("does not justify"));
    assert!(!serde_json::to_string(&insight).unwrap().contains("private-compiled-canary"));
}

#[test]
fn native_integer_and_cache_facts_refine_context_without_turning_co_occurrence_into_a_reuse_cause() {
    for cached in [true, false] {
        let mut plan = parameterized_seek();
        let context = plan.facts_mut().parameter_context.as_mut().unwrap();
        context.retrieved_from_cache = Some(cached);
        context.parameters[0].integer_values_equal = Some(false);
        let insight = context_insight(&plan).unwrap();
        assert!(insight.detail.contains(&if cached {
            t!("insight.parameter_context.cache.used")
        } else {
            t!("insight.parameter_context.cache.not_used")
        }));
        assert!(
            insight
                .detail
                .contains(&t!("insight.parameter_context.integer.different", parameter = parameter_label_for_test()))
        );
        assert_eq!(insight.severity, Severity::Low);
        assert_eq!(insight.impact, 0.0);
        assert!(insight.fix_sql.is_none());
        assert_eq!(insight.fix, t!("insight.parameter_context.fix"));
    }
}

#[test]
fn numerically_equal_native_integers_do_not_gain_a_different_input_access_binding_from_lexical_difference() {
    let mut plan = parameterized_seek();
    plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0].integer_values_equal = Some(true);
    let insight = context_insight(&plan).unwrap();
    assert!(
        insight.detail.contains(&t!("insight.parameter_context.integer.same", parameter = parameter_label_for_test()))
    );
    assert!(!insight.detail.contains(&t!(
        "insight.parameter_context.access",
        node = "0",
        parameter = parameter_label_for_test()
    )));
    assert_eq!(insight.nodes, vec![plan.root.id]);
    assert!(insight.fix_sql.is_none());
}

#[test]
fn equal_missing_redacted_estimated_or_unexecuted_context_never_becomes_a_difference_insight() {
    for state in ["equal", "missing", "redacted", "estimated", "generic", "no-runtime", "invalid-runtime"] {
        let mut plan = recorded_plan();
        let parameter = &mut plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0];
        match state {
            "equal" => parameter.comparison = Comparison::SameRepresentation,
            "missing" => {
                parameter.runtime = State::NotRecorded;
                parameter.comparison = Comparison::Unavailable;
            }
            "redacted" => {
                parameter.runtime = State::Redacted;
                parameter.comparison = Comparison::Unavailable;
            }
            "estimated" => plan.kind = PlanKind::Estimated,
            "generic" => plan.kind = PlanKind::Generic,
            "no-runtime" => plan.root.act_rows = None,
            "invalid-runtime" => plan.root.act_executions = Some(f64::NAN),
            _ => unreachable!(),
        }
        assert!(context_insight(&plan).is_none(), "{state}");
    }
}

#[test]
fn parameter_examples_are_bounded_and_unknown_types_are_never_filled_from_other_entries() {
    let mut plan = recorded_plan();
    {
        let context = plan.facts_mut().parameter_context.as_mut().unwrap();
        context.parameters = (0..10).map(|index| parameter(&format!("@parameter_{index}"))).collect();
        context.parameters[0].data_type = None;
    }
    let insight = context_insight(&plan).unwrap();
    assert!(insight.detail.contains(&t!("insight.parameter_context.unknown_type", parameter = "@parameter_0")));
    assert!(insight.detail.contains("@parameter_3"));
    assert!(!insight.detail.contains("@parameter_4"));
    let examples: Vec<_> = plan.facts().parameter_context.as_ref().unwrap().parameters[..4]
        .iter()
        .map(|parameter| match parameter.data_type.as_deref() {
            Some(data_type) => t!(
                "insight.parameter_context.named_type",
                parameter = parameter.name.as_deref().unwrap(),
                data_type = data_type
            ),
            None => t!("insight.parameter_context.unknown_type", parameter = parameter.name.as_deref().unwrap()),
        })
        .collect();
    assert_eq!(
        insight.detail,
        t!("insight.parameter_context.detail", count = "10", parameters = format::list(&examples))
    );
}

#[test]
fn duplicate_names_cannot_gain_comparison_authority_from_an_in_memory_or_saved_plan() {
    let mut plan = recorded_plan();
    let context = plan.facts_mut().parameter_context.as_mut().unwrap();
    context.parameters.push(parameter("@customer"));
    context.issues.push(PlanParameterIssue::DuplicateName);
    assert!(context_insight(&plan).is_none(), "duplicate native names have no representative comparison");
    for parameter in &mut plan.facts_mut().parameter_context.as_mut().unwrap().parameters {
        parameter.comparison = Comparison::Unavailable;
    }
    assert!(context_insight(&plan).is_none());
}

#[test]
fn constant_redaction_preserves_only_the_safe_precomputed_comparison_and_redacts_type_context() {
    let mut plan = recorded_plan();
    plan.statement = Some("SELECT value FROM dbo.rows WHERE customer = N'private-value-canary'".into());
    plan.facts_mut().compiled_parameters = Some("@customer = N'private-value-canary'".into());
    plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0].data_type =
        Some("[private-database-canary].[dbo].[CustomerType]".into());
    annotate(&mut plan);
    let redacted = crate::redact::redact_plan(&plan);
    let context = redacted.facts().parameter_context.as_ref().unwrap();
    assert!(context.is_valid());
    assert_eq!(context.parameters[0].comparison, Comparison::DifferentRepresentation);
    assert_eq!(context.parameters[0].data_type.as_deref(), Some("[dbo].[CustomerType]"));
    assert!(context_insight(&redacted).is_some());
    let json = serde_json::to_value(&redacted).unwrap();
    let text = json.to_string();
    assert!(!text.contains("private-value-canary") && !text.contains("private-database-canary"));
    assert_eq!(json, serde_json::to_value(crate::redact::redact_plan(&redacted)).unwrap());
}

#[test]
fn redaction_that_withholds_an_invalid_parameter_name_also_withholds_its_comparison() {
    let mut plan = recorded_plan();
    plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0].name =
        Some("@invalid N'private-name-canary'".into());
    let redacted = crate::redact::redact_plan(&plan);
    let parameter = &redacted.facts().parameter_context.as_ref().unwrap().parameters[0];
    assert!(parameter.name.is_none());
    assert_eq!(parameter.comparison, Comparison::Unavailable);
    assert_eq!(redacted.root.act_rows, plan.root.act_rows);
    assert!(context_insight(&redacted).is_none());
    assert!(!serde_json::to_string(&redacted).unwrap().contains("private-name-canary"));
}

fn parameterized_seek() -> Plan {
    use crate::plan_facts::{PlanAccessRows, PlanParameterUse, PlanParameterUseKind};
    let mut plan = recorded_plan();
    plan.root.kind = NodeKind::Seek;
    plan.root.op = "Index Seek".into();
    plan.root.object = Some("dbo.orders".into());
    plan.root.index = Some("ix_customer".into());
    plan.root.seek_predicate = Some("orders.customer = @customer".into());
    plan.root.est_rows = 100.0;
    plan.root.act_rows = Some(10_000.0);
    plan.root.facts_mut().serial_execution_count = Some("1".into());
    plan.root.facts_mut().access_rows = PlanAccessRows::new(Some(100.0), Some(10_000.0));
    let node_id = plan.root.id;
    plan.facts_mut().parameter_context.as_mut().unwrap().uses =
        vec![PlanParameterUse { parameter_index: 0, node_id, kind: PlanParameterUseKind::EqualitySeek }];
    plan
}

#[test]
fn exact_parameterized_access_mismatch_is_observed_context_with_no_automatic_statistics_or_recompile_advice() {
    let plan = parameterized_seek();
    let insight = context_insight(&plan).unwrap();
    assert_eq!(insight.nodes, vec![plan.root.id]);
    assert!(insight.detail.contains(&t!(
        "insight.parameter_context.access",
        node = "0",
        parameter = parameter_label_for_test()
    )));
    assert!(insight.detail.contains(&t!("insight.relational_input.access_counts", estimated = "100", actual = "10K")));
    assert_eq!(insight.impact, 0.0);
    assert!(insight.fix_sql.is_none());
    let misestimate = insights(&plan).into_iter().find(|insight| insight.rule == "misestimate").unwrap();
    assert!(misestimate.fix_sql.is_none());
    assert_eq!(misestimate.fix, t!("insight.parameter_context.access_fix"));
}

fn parameter_label_for_test() -> String {
    t!("insight.parameter_context.named_type", parameter = "@customer", data_type = "int")
}

#[test]
fn healthy_unbound_ambiguous_parallel_or_incomplete_access_never_acquires_parameterized_mismatch_context() {
    for case in [
        "healthy",
        "overestimate",
        "unused",
        "same",
        "parallel",
        "serial-unknown",
        "output-missing",
        "access-missing",
        "wrong-node",
        "duplicate-node",
        "use-limit",
        "relational-input",
    ] {
        let mut plan = parameterized_seek();
        match case {
            "healthy" => {
                plan.root.est_rows = 10_000.0;
                plan.root.facts_mut().access_rows.as_mut().unwrap().estimated_per_execution = Some(10_000.0);
            }
            "overestimate" => {
                plan.root.est_rows = 1_000_000.0;
                plan.root.facts_mut().access_rows.as_mut().unwrap().estimated_per_execution = Some(1_000_000.0);
            }
            "unused" => plan.facts_mut().parameter_context.as_mut().unwrap().uses.clear(),
            "same" => {
                plan.facts_mut().parameter_context.as_mut().unwrap().parameters[0].comparison =
                    Comparison::SameRepresentation
            }
            "parallel" => plan.root.facts_mut().parallel_threads = Some(2),
            "serial-unknown" => plan.root.facts_mut().serial_execution_count = None,
            "output-missing" => plan.root.act_rows = None,
            "access-missing" => plan.root.facts_mut().access_rows.as_mut().unwrap().actual_total = None,
            "wrong-node" => plan.facts_mut().parameter_context.as_mut().unwrap().uses[0].node_id = 99,
            "duplicate-node" => {
                let mut copy = plan.root.clone();
                copy.children.clear();
                plan.root.children.push(copy);
            }
            "use-limit" => {
                let context = plan.facts_mut().parameter_context.as_mut().unwrap();
                context.uses.clear();
                context.issues.push(PlanParameterIssue::UseLimit);
            }
            "relational-input" => {
                plan.root.facts_mut().relational_input = Some(crate::plan_facts::RelationalInput {
                    kind: crate::plan_facts::RelationalInputKind::Unknown,
                    provenance: crate::plan_facts::RelationalInputProvenance::Unresolved,
                })
            }
            _ => unreachable!(),
        }
        if let Some(insight) = context_insight(&plan) {
            assert!(
                !insight.detail.contains(&t!(
                    "insight.parameter_context.access",
                    node = "0",
                    parameter = parameter_label_for_test()
                )),
                "{case}"
            );
        }
    }
}
