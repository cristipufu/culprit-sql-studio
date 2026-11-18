use super::*;
use culprit_core::{MemoryParticipantExplanation, telemetry::*};

fn original_grant() -> ExecutionGrantObservation {
    let grant: ExecutionGrantObservation = serde_json::from_value(json!({
        "source":"current request memory", "identity":NativeIdentity::new("42:0"),
        "observed_at":"2020-01-01T12:00:00Z", "state":"granted", "request_association":"generation-checked",
        "statement_relationship":"observed-current-statement", "scope":"database",
        "requested_without_offset":"2020-01-01T10:00:00.003", "granted_without_offset":"2020-01-01T10:00:00.006",
        "queue":ExecutionGrantQueue::default(),"native_properties":{},"available_columns":[],"native_null_columns":[],
        "request":{
            "key":{"session":{"id":"42","generation":"native-login"},"request":{"id":"0","generation":"native-request"}},
            "blocking_status":"known","idle":false,"scope":"request-database","state":"running","database":"db",
            "session_counters":{},"request_counters":{},"statement_identity":NativeIdentity::new("native-statement"),
            "statement":DefinitionPayload::Unavailable{reason:PayloadUnavailable::NotCollected}
        }
    })).unwrap();
    assert!(grant.is_valid());
    grant
}
fn explanation(grant: &ExecutionGrantObservation) -> MemoryParticipantExplanation {
    MemoryParticipantExplanation {
        observation_key: grant.key().unwrap(),
        observation_identity: grant.identity.clone(),
        qualified_request: grant.request.as_ref().map(|request| request.key.clone()),
        mechanism: None,
        title: String::new(),
        explanation: String::new(),
        investigation: String::new(),
        verify: String::new(),
        grant_admission: None,
    }
}

#[test]
fn an_unqualified_request_keeps_its_original_raw_fact_but_cannot_supply_positive_context() {
    let grant = original_grant();
    let original = grant.clone();
    let mut entry = explanation(&grant);
    assert!(std::ptr::eq(qualified_grant_request(&entry, &grant).unwrap().unwrap(), grant.request.as_ref().unwrap()));
    entry.qualified_request = None;
    assert!(qualified_grant_request(&entry, &grant).unwrap().is_none());
    assert_eq!(grant, original);
}

#[test]
fn a_qualified_reference_cannot_select_another_session_request_transaction_or_native_grant() {
    let grant = original_grant();
    for field in ["login", "request-id", "request-generation", "transaction", "grant"] {
        let mut entry = explanation(&grant);
        match field {
            "login" => entry.qualified_request.as_mut().unwrap().session.generation = "foreign-login".into(),
            "request-id" => entry.qualified_request.as_mut().unwrap().request.as_mut().unwrap().id = "1".into(),
            "request-generation" => {
                entry.qualified_request.as_mut().unwrap().request.as_mut().unwrap().generation =
                    "foreign-request".into()
            }
            "transaction" => entry.qualified_request.as_mut().unwrap().transaction = Some("foreign-transaction".into()),
            _ => entry.observation_identity.id = "43:0".into(),
        }
        assert_eq!(
            qualified_grant_request(&entry, &grant).unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{field}"
        );
    }
    let entry = explanation(&grant);
    let mut missing = grant;
    missing.request = None;
    assert!(qualified_grant_request(&entry, &missing).is_err());
}
