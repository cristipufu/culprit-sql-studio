use super::*;
use culprit_core::{MeasurementScope, SourceState, telemetry::*};
use serde_json::json;

pub(crate) fn observation(id: &str, state: ExecutionGrantState) -> ExecutionGrantObservation {
    let row = serde_json::from_value(json!({
        "source":"current request memory",
        "identity":{"id":id,"qualifiers":{"requested_without_offset":"2020-01-01T10:00:00.003"}},
        "observed_at":"2020-01-01T08:01:00Z", "state":state,
        "request_association":"missing-request", "statement_relationship":"unresolved",
        "requested_without_offset":"2020-01-01T10:00:00.003",
        "granted_without_offset":(state==ExecutionGrantState::Granted).then_some("2020-01-01T10:00:00.006"),
        "requested_kibibytes":"9007199254740993", "granted_kibibytes":"1024", "used_kibibytes":"0",
        "native_properties":{}, "available_columns":[], "native_null_columns":[]
    }))
    .unwrap();
    assert!(ExecutionGrantObservation::is_valid(&row));
    row
}

pub(crate) fn associated(row: &mut ExecutionGrantObservation) {
    row.request_association = RequestGrantAssociation::GenerationChecked;
    row.statement_relationship = GrantStatementRelationship::ObservedCurrentStatement;
    row.scope = Some(MeasurementScope::Database);
    row.request = Some(serde_json::from_value(json!({
        "key":{"session":{"id":"61","generation":"login-1"},"request":{"id":"0","generation":"request-1"}},
        "blocking_status":"known","idle":false,"scope":"request-database", "state":"running", "database":"db",
        "open_transactions":0,"session_counters":{},"request_counters":{},
        "statement":DefinitionPayload::sql("SELECT payload FROM dbo.orders WHERE customer_id=9264", culprit_core::Engine::Mssql)
    })).unwrap());
    assert!(row.is_valid());
}

fn coverage(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: "current request memory".into(), state, reason: None, reason_message: None }
}

#[test]
fn waiting_and_reserved_participants_keep_exact_native_values_without_a_waste_or_owner_verdict() {
    let mut waiter = observation("waiter", ExecutionGrantState::Waiting);
    waiter.granted_kibibytes = None;
    waiter.used_kibibytes = None;
    waiter.wait_ms = Some("9007199254740993".into());
    associated(&mut waiter);
    let holder = observation("holder", ExecutionGrantState::Granted);
    let report = explain(&[&holder, &waiter], Some(&coverage(SourceState::Partial))).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.entries[0].observation_key, waiter.key().unwrap());
    assert_eq!(report.entries[0].observation_identity, waiter.identity);
    assert_eq!(report.entries[1].observation_key, holder.key().unwrap());
    assert!(report.entries[0].title.contains("9007199254740993"));
    assert!(report.entries[0].explanation.contains("9007199254740993 ms"));
    assert!(report.entries[1].explanation.contains("0 KiB"));
    assert!(report.entries[1].explanation.contains(&t!("view.request_memory.granted.explanation")));
    assert!(report.limitations.contains(&t!("view.request_memory.limit.unowned")));
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("SELECT payload"), "the explanation references one original native row");
    assert!(!json.contains("9264"));
    for entry in &report.entries {
        assert!(!entry.investigation.is_empty() && !entry.verify.is_empty());
    }
}

#[test]
fn missing_changed_and_unqualified_requests_remain_distinct_from_observed_current_sql() {
    for (association, message) in [
        (RequestGrantAssociation::MissingRequest, "view.request_memory.request.missing"),
        (RequestGrantAssociation::ChangedRequest, "view.request_memory.request.changed"),
        (RequestGrantAssociation::InsufficientEvidence, "view.request_memory.request.unqualified"),
    ] {
        let mut row = observation("unowned", ExecutionGrantState::Granted);
        row.request_association = association;
        let report = explain(&[&row], None).unwrap();
        assert!(report.entries[0].explanation.contains(&culprit_core::i18n::t(message, &[])));
        assert!(report.limitations.contains(&t!("view.request_memory.limit.unowned")));
        assert_eq!(row.scope, None);
    }
    let mut row = observation("observed", ExecutionGrantState::Granted);
    associated(&mut row);
    let report = explain(&[&row], None).unwrap();
    assert!(report.entries[0].explanation.contains(&t!("view.request_memory.request.observed_statement")));
    assert!(!report.limitations.contains(&t!("view.request_memory.limit.unowned")));
    row.statement_relationship = GrantStatementRelationship::Unresolved;
    row.request.as_mut().unwrap().statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    let report = explain(&[&row], None).unwrap();
    assert!(report.entries[0].explanation.contains(&t!("view.request_memory.request.statement_unavailable")));
}

#[test]
fn empty_partial_unavailable_and_unknown_source_do_not_claim_healthy_history() {
    for state in [SourceState::Empty, SourceState::Partial, SourceState::Unavailable, SourceState::NotApplicable] {
        let source = coverage(state);
        let report = explain(&[], Some(&source)).unwrap();
        assert_eq!(report.source_state, Some(state));
        assert!(report.entries.is_empty());
        assert_eq!(report.summary, t!("view.request_memory.summary.empty"));
        assert!(report.limitations.contains(&t!("view.request_memory.limit.observation")));
    }
    let report = explain(&[], None).unwrap();
    assert!(report.limitations.contains(&t!("view.memory_participants.limit.source_unknown")));
}

#[test]
fn conflicting_sources_duplicate_grants_and_invalid_native_values_are_rejected() {
    let row = observation("same-native-grant", ExecutionGrantState::Granted);
    let mut changed = row.clone();
    changed.used_kibibytes = Some("512".into());
    assert!(explain(&[&row, &changed], None).is_err(), "mutable counters cannot invent a second grant identity");
    let mut foreign = row.clone();
    foreign.source = "another native source".into();
    assert!(explain(&[&row, &foreign], None).is_err());
    assert!(explain(&[&row], Some(&coverage(SourceState::Empty))).is_err());
    assert!(explain(&[], Some(&coverage(SourceState::Collected))).is_err());
    changed.used_kibibytes = Some("-1".into());
    assert!(explain(&[&changed], None).is_err());
    let mut source = coverage(SourceState::Collected);
    source.source = "wrong source".into();
    assert!(explain(&[&row], Some(&source)).is_err());
}

#[test]
fn native_grant_order_is_deterministic_and_peak_memory_never_becomes_current_usage() {
    let first = observation("first", ExecutionGrantState::Granted);
    let mut second = observation("second", ExecutionGrantState::Granted);
    second.requested_kibibytes = Some("2048".into());
    second.maximum_used_kibibytes = Some("9007199254740993".into());
    second.used_kibibytes = None;
    let forward = explain(&[&first, &second], None).unwrap();
    let reverse = explain(&[&second, &first], None).unwrap();
    assert_eq!(forward, reverse);
    let entry = forward.entries.iter().find(|entry| entry.observation_key == second.key().unwrap()).unwrap();
    assert!(!entry.explanation.contains(&t!("view.request_memory.metric.used", n = "0")));
    assert!(!entry.explanation.contains("9007199254740993"));
}
