use super::*;

fn parameter() -> PlanParameter {
    PlanParameter {
        integer_values_equal: None,
        name: Some("@customer".into()),
        data_type: Some("nvarchar(4000)".into()),
        compiled: PlanParameterValueState::Recorded,
        runtime: PlanParameterValueState::Recorded,
        comparison: PlanParameterComparison::DifferentRepresentation,
    }
}

#[test]
fn safe_parameter_context_round_trips_without_a_literal_or_value_hash_field() {
    let context = PlanParameterContext {
        retrieved_from_cache: None,
        parameters: vec![parameter()],
        uses: vec![],
        issues: vec![],
    };
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
    let mut context = PlanParameterContext {
        retrieved_from_cache: None,
        parameters: vec![parameter(), parameter()],
        uses: vec![],
        issues: vec![],
    };
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
        retrieved_from_cache: None,
        parameters: (0..MAX_PLAN_PARAMETERS)
            .map(|index| PlanParameter {
                integer_values_equal: None,
                name: Some(format!("@p{index}")),
                comparison: PlanParameterComparison::Unavailable,
                ..parameter()
            })
            .collect(),
        uses: vec![],
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
        let mut context = serde_json::to_value(PlanParameterContext {
            retrieved_from_cache: None,
            parameters: vec![parameter()],
            uses: vec![],
            issues: vec![],
        })
        .unwrap();
        context["parameters"][0][field] = serde_json::json!("private\ninvalid-metadata");
        facts["parameter_context"] = context;
        let parsed: crate::PlanFacts = serde_json::from_value(facts).unwrap();
        assert!(parsed.parameter_context.is_none());
    }
}

#[test]
fn native_use_rosters_require_unique_nodes_and_exact_nonduplicate_parameter_indices() {
    let usage = PlanParameterUse { parameter_index: 0, node_id: 7, kind: PlanParameterUseKind::EqualitySeek };
    let mut context = PlanParameterContext {
        retrieved_from_cache: None,
        parameters: vec![parameter()],
        uses: vec![usage],
        issues: vec![],
    };
    assert!(context.is_valid());
    context.uses.push(usage);
    assert!(!context.is_valid());
    context.uses = vec![PlanParameterUse { parameter_index: 1, ..usage }];
    assert!(!context.is_valid());
    context.uses = vec![usage];
    context.parameters.push(parameter());
    context.issues.push(PlanParameterIssue::DuplicateName);
    for parameter in &mut context.parameters {
        parameter.comparison = PlanParameterComparison::Unavailable;
    }
    assert!(!context.is_valid(), "a duplicate native name cannot supply a representative use");
}

#[test]
fn explicit_population_limits_cannot_keep_positive_use_or_parameter_subsets() {
    let mut context = PlanParameterContext {
        retrieved_from_cache: None,
        parameters: vec![parameter()],
        uses: vec![PlanParameterUse { parameter_index: 0, node_id: 7, kind: PlanParameterUseKind::EqualitySeek }],
        issues: vec![PlanParameterIssue::UseLimit],
    };
    assert!(!context.is_valid());
    context.uses.clear();
    assert!(context.is_valid());
    context.issues = vec![PlanParameterIssue::MetadataLimit];
    assert!(!context.is_valid());
    context.parameters.clear();
    assert!(context.is_valid());
}

#[test]
fn an_integer_comparison_requires_recorded_attributes_and_cannot_contradict_identical_native_text() {
    let mut entry = parameter();
    entry.data_type = Some("int".into());
    entry.integer_values_equal = Some(false);
    assert!(entry.is_valid());
    entry.comparison = PlanParameterComparison::SameRepresentation;
    assert!(!entry.is_valid());
    entry.integer_values_equal = Some(true);
    assert!(entry.is_valid());
    for missing in ["name", "type", "compiled", "runtime", "comparison"] {
        let mut invalid = entry.clone();
        match missing {
            "name" => invalid.name = None,
            "type" => invalid.data_type = None,
            "compiled" => invalid.compiled = PlanParameterValueState::Redacted,
            "runtime" => invalid.runtime = PlanParameterValueState::NotRecorded,
            _ => invalid.comparison = PlanParameterComparison::Unavailable,
        }
        assert!(!invalid.is_valid(), "{missing}");
    }
}

#[test]
fn native_cache_verdict_is_bounded_context_but_cannot_survive_an_unbound_statement() {
    let mut context = PlanParameterContext {
        retrieved_from_cache: Some(true),
        parameters: vec![parameter()],
        uses: vec![],
        issues: vec![],
    };
    assert!(context.is_valid());
    let value = serde_json::to_value(&context).unwrap();
    assert_eq!(value["retrieved_from_cache"], true);
    assert_eq!(serde_json::from_value::<PlanParameterContext>(value).unwrap(), context);
    context.parameters[0].comparison = PlanParameterComparison::Unavailable;
    context.parameters[0].runtime = PlanParameterValueState::Unqualified;
    context.issues.push(PlanParameterIssue::UnboundStatement);
    assert!(!context.is_valid());
    context.retrieved_from_cache = None;
    assert!(context.is_valid());
}
