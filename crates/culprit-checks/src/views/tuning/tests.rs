use super::*;
use culprit_core::{
    QueryStoreScope,
    telemetry::{NativeForcedPlan, TuningFact, TuningObservation},
};

fn forced(scope: QueryStoreScope) -> TuningObservation {
    TuningObservation {
        source: "forced plans".into(),
        fact: TuningFact::ForcedPlan(NativeForcedPlan {
            plan_scope: scope,
            query_id: "3".into(),
            plan_id: "9".into(),
            failures: "9007199254740993".into(),
            last_failure_reason: Some("NO_PLAN".into()),
            last_execution_at: None,
            forcing: None,
            reset: culprit_core::ResetMarker::Unknown,
        }),
    }
}
fn outcome(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: "forced plans".into(), state, reason: None, reason_message: None }
}
#[test]
fn independent_native_scope_and_exact_counter_arguments_do_not_authorize_a_command_or_workload_join() {
    let original = forced(QueryStoreScope::Unknown);
    let known = forced(QueryStoreScope::ReplicaGroup { id: "7".into() });
    let report = explain(&[&original, &known], &[outcome(SourceState::Partial)]).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert!(report.entries.iter().any(|entry| entry.scope == Some(QueryStoreScope::Unknown)));
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(serialized.contains("9007199254740993"));
    assert!(!serialized.contains("fix_sql") && !serialized.contains("EXEC "));
    assert!(
        report
            .limitations
            .iter()
            .any(|message| matches!(message, Message::Catalog {key,..} if key=="view.tuning.limit.scope"))
    );
}
#[test]
fn empty_unavailable_and_partial_sources_keep_their_distinct_outcomes() {
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::Partial, SourceState::NotApplicable] {
        let report = explain(&[], &[outcome(state)]).unwrap();
        assert!(report.entries.is_empty());
        assert_eq!(report.sources[0].state, state);
    }
    assert!(explain(&[], &[outcome(SourceState::Collected)]).is_err());
}
#[test]
fn contradictory_source_states_duplicate_native_keys_and_invalid_counts_are_rejected() {
    let row = forced(QueryStoreScope::Local);
    assert!(explain(&[&row], &[outcome(SourceState::Unavailable)]).is_err());
    assert!(explain(&[&row, &row], &[outcome(SourceState::Partial)]).is_err());
    let mut invalid = row;
    let TuningFact::ForcedPlan(value) = &mut invalid.fact else { unreachable!() };
    value.failures = "-1".into();
    assert!(explain(&[&invalid], &[outcome(SourceState::Partial)]).is_err());
}
