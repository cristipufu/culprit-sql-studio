use super::*;

fn parameter() -> PlanParameter {
    PlanParameter {
        name: Some("@customer".into()),
        data_type: Some("nvarchar(4000)".into()),
        compiled: PlanParameterValueState::Recorded,
        runtime: PlanParameterValueState::Recorded,
        comparison: PlanParameterComparison::DifferentRepresentation,
    }
}

#[test]
fn safe_parameter_context_round_trips_without_a_literal_or_value_hash_field() {
    let context = PlanParameterContext { parameters: vec![parameter()], issues: vec![] };
    assert!(context.is_valid());
    let value = serde_json::to_value(&context).unwrap();
    let fields = value["parameters"][0].as_object().unwrap();
    assert_eq!(fields.len(), 5);
    assert!(fields.contains_key("name") && fields.contains_key("data_type"));
    assert_eq!(fields["comparison"], "different-representation");
    assert_eq!(serde_json::from_value::<PlanParameterContext>(value).unwrap(), context);
}

#[test]
fn unavailable_or_unbound_native_attributes_cannot_authorize_a_representation_comparison() {
    for state in
        [PlanParameterValueState::NotRecorded, PlanParameterValueState::Redacted, PlanParameterValueState::Unqualified]
    {
        for compiled in [true, false] {
            let mut entry = parameter();
            if compiled {
                entry.compiled = state
            } else {
                entry.runtime = state
            }
            assert!(!entry.is_valid());
            entry.comparison = PlanParameterComparison::Unavailable;
            assert!(entry.is_valid());
        }
    }
    let mut entry = parameter();
    entry.name = None;
    assert!(!entry.is_valid());
    entry.comparison = PlanParameterComparison::Unavailable;
    assert!(entry.is_valid());
}

#[test]
fn duplicate_names_require_an_explicit_issue_without_a_representative_comparison() {
    let mut context = PlanParameterContext { parameters: vec![parameter(), parameter()], issues: vec![] };
    assert!(!context.is_valid());
    context.issues.push(PlanParameterIssue::DuplicateName);
    assert!(!context.is_valid());
    for parameter in &mut context.parameters {
        parameter.comparison = PlanParameterComparison::Unavailable;
    }
    assert!(context.is_valid());
    context.issues.push(PlanParameterIssue::DuplicateName);
    assert!(!context.is_valid());
}

#[test]
fn bounded_partial_parameter_context_cannot_claim_comparisons_past_unread_metadata() {
    let mut context = PlanParameterContext {
        parameters: (0..MAX_PLAN_PARAMETERS)
            .map(|index| PlanParameter {
                name: Some(format!("@p{index}")),
                comparison: PlanParameterComparison::Unavailable,
                ..parameter()
            })
            .collect(),
        issues: vec![PlanParameterIssue::ParameterLimit],
    };
    assert!(context.is_valid());
    context.parameters[0].comparison = PlanParameterComparison::DifferentRepresentation;
    assert!(!context.is_valid());
    context.parameters[0].comparison = PlanParameterComparison::Unavailable;
    context.parameters.push(parameter());
    assert!(!context.is_valid());
}

#[test]
fn invalid_parameter_metadata_does_not_survive_normalized_plan_deserialization_as_proof() {
    for field in ["name", "data_type"] {
        let mut facts = serde_json::to_value(crate::PlanFacts::default()).unwrap();
        let mut context =
            serde_json::to_value(PlanParameterContext { parameters: vec![parameter()], issues: vec![] }).unwrap();
        context["parameters"][0][field] = serde_json::json!("private\ninvalid-metadata");
        facts["parameter_context"] = context;
        let parsed: crate::PlanFacts = serde_json::from_value(facts).unwrap();
        assert!(parsed.parameter_context.is_none());
    }
}
