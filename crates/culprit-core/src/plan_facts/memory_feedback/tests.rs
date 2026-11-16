use super::*;

fn recorded() -> PlanMemoryGrantFeedback {
    PlanMemoryGrantFeedback {
        state: PlanMemoryGrantFeedbackState::DisabledUnstable,
        previous_grant_kb: Some("9007199254740993".into()),
        runtime_observed: true,
        issues: vec![],
    }
}

#[test]
fn exact_native_prior_grants_round_trip_without_floating_point_or_current_memory_fields() {
    for amount in ["0", "9007199254740993", "18446744073709551615"] {
        let mut feedback = recorded();
        feedback.previous_grant_kb = Some(amount.into());
        assert!(feedback.is_valid());
        let value = serde_json::to_value(&feedback).unwrap();
        assert_eq!(value["previous_grant_kb"], amount);
        assert_eq!(value["state"], "disabled-unstable");
        assert_eq!(value.as_object().unwrap().len(), 4);
        assert_eq!(serde_json::from_value::<PlanMemoryGrantFeedback>(value).unwrap(), feedback);
    }
}

#[test]
fn absent_unrecognized_and_malformed_native_fields_remain_different() {
    let missing = PlanMemoryGrantFeedback {
        state: PlanMemoryGrantFeedbackState::NotRecorded,
        previous_grant_kb: Some("0".into()),
        runtime_observed: true,
        issues: vec![],
    };
    let unknown = PlanMemoryGrantFeedback {
        state: PlanMemoryGrantFeedbackState::Unrecognized,
        previous_grant_kb: None,
        runtime_observed: true,
        issues: vec![],
    };
    let malformed = PlanMemoryGrantFeedback {
        issues: vec![PlanMemoryGrantFeedbackIssue::InvalidState, PlanMemoryGrantFeedbackIssue::InvalidPreviousGrant],
        ..unknown.clone()
    };
    assert!(missing.is_valid() && unknown.is_valid() && malformed.is_valid());
    assert_ne!(missing, unknown);
    assert_ne!(unknown, malformed);
    assert!(crate::PlanFacts::default().memory_grant_feedback.is_none());
}

#[test]
fn contradictory_issues_or_noncanonical_counts_cannot_survive_as_normalized_evidence() {
    for value in ["", "01", "-1", "+1", "1.5", "1e3", "NaN", "18446744073709551616"] {
        let mut feedback = recorded();
        feedback.previous_grant_kb = Some(value.into());
        assert!(!feedback.is_valid(), "{value}");
        let mut facts = serde_json::to_value(crate::PlanFacts::default()).unwrap();
        facts["memory_grant_feedback"] = serde_json::to_value(feedback).unwrap();
        let restored: crate::PlanFacts = serde_json::from_value(facts).unwrap();
        assert!(restored.memory_grant_feedback.is_none(), "{value}");
    }
    for issues in [
        vec![PlanMemoryGrantFeedbackIssue::InvalidState],
        vec![PlanMemoryGrantFeedbackIssue::InvalidPreviousGrant],
        vec![PlanMemoryGrantFeedbackIssue::AmbiguousMetadata],
        vec![PlanMemoryGrantFeedbackIssue::InvalidState, PlanMemoryGrantFeedbackIssue::InvalidState],
    ] {
        let mut feedback = recorded();
        feedback.issues = issues;
        assert!(!feedback.is_valid());
    }
}

#[test]
fn ambiguous_native_metadata_cannot_choose_a_state_or_prior_grant_representative() {
    let mut feedback = PlanMemoryGrantFeedback {
        state: PlanMemoryGrantFeedbackState::Unrecognized,
        previous_grant_kb: None,
        runtime_observed: false,
        issues: vec![PlanMemoryGrantFeedbackIssue::AmbiguousMetadata],
    };
    assert!(feedback.is_valid());
    feedback.runtime_observed = true;
    assert!(!feedback.is_valid());
    feedback.runtime_observed = false;
    feedback.previous_grant_kb = Some("0".into());
    assert!(!feedback.is_valid());
}
