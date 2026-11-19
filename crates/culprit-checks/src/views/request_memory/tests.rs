use super::*;
use culprit_core::{MeasurementScope, SourceState, telemetry::*};
use serde_json::json;
mod admission;
mod output_wait;

pub(crate) fn observation(id: &str, state: ExecutionGrantState) -> ExecutionGrantObservation {
    let row = serde_json::from_value(json!({
        "source":"current request memory",
        "identity":{"id":id,"qualifiers":{"requested_without_offset":"2020-01-01T10:00:00.003"}},
        "observed_at":"2020-01-01T08:01:00Z", "state":state,
        "request_association":"missing-request", "statement_relationship":"unresolved",
        "requested_without_offset":"2020-01-01T10:00:00.003",
        "granted_without_offset":(state==ExecutionGrantState::Granted).then_some("2020-01-01T10:00:00.006"),
        "requested_kibibytes":"9007199254740993", "granted_kibibytes":"1024", "used_kibibytes":"0",
        "resource_observation":null, "queue":ExecutionGrantQueue::default(), "native_properties":{}, "available_columns":[], "native_null_columns":[]
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
        "key":{"session":{"id":row.identity.id,"generation":"login-1"},"request":{"id":"0","generation":"request-1"}},
        "blocking_status":"known","idle":false,"scope":"request-database", "state":"running", "database":"db",
        "open_transactions":0,"session_counters":{},"request_counters":{},
        "statement_identity":NativeIdentity::new(format!("statement-{}",row.identity.id)),
        "statement":DefinitionPayload::sql("SELECT payload FROM dbo.orders WHERE customer_id=9264", culprit_core::Engine::Mssql)
    })).unwrap());
    assert!(row.is_valid());
}

pub(crate) fn context() -> NativeReadContext {
    NativeReadContext {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Node,
                namespace: NativeIdentity::new("synthetic-logical-db"),
                native: NativeIdentity::new("synthetic-native-member"),
            }),
            epoch: Some("synthetic-native-epoch".into()),
        },
        observed_from: "2020-01-01T08:01:00Z".into(),
        observed_until: "2020-01-01T08:01:00Z".into(),
        database_writable: Some(true),
    }
}
fn explain_read(
    rows: &[&ExecutionGrantObservation],
    source: Option<&SourceOutcome>,
) -> Result<MemoryParticipantsReport> {
    super::explain(RequestMemoryEvidence { context: &context(), observations: rows }, source)
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
    let report = explain_read(&[&holder, &waiter], Some(&coverage(SourceState::Partial))).unwrap();
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
        let report = explain_read(&[&row], None).unwrap();
        assert!(report.entries[0].explanation.contains(&culprit_core::i18n::t(message, &[])));
        assert!(report.limitations.contains(&t!("view.request_memory.limit.unowned")));
        assert_eq!(row.scope, None);
    }
    let mut row = observation("observed", ExecutionGrantState::Granted);
    associated(&mut row);
    let report = explain_read(&[&row], None).unwrap();
    assert!(report.entries[0].explanation.contains(&t!("view.request_memory.request.observed_statement")));
    assert!(!report.limitations.contains(&t!("view.request_memory.limit.unowned")));
    row.statement_relationship = GrantStatementRelationship::Unresolved;
    row.request.as_mut().unwrap().statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    let report = explain_read(&[&row], None).unwrap();
    assert!(report.entries[0].explanation.contains(&t!("view.request_memory.request.statement_unavailable")));
}

#[test]
fn empty_partial_unavailable_and_unknown_source_do_not_claim_healthy_history() {
    for state in [SourceState::Empty, SourceState::Partial, SourceState::Unavailable, SourceState::NotApplicable] {
        let source = coverage(state);
        let report = explain_read(&[], Some(&source)).unwrap();
        assert_eq!(report.source_state, Some(state));
        assert!(report.entries.is_empty());
        assert_eq!(report.summary, t!("view.request_memory.summary.empty"));
        assert!(report.limitations.contains(&t!("view.request_memory.limit.observation")));
    }
    let report = explain_read(&[], None).unwrap();
    assert!(report.limitations.contains(&t!("view.memory_participants.limit.source_unknown")));
}

#[test]
fn conflicting_sources_duplicate_grants_and_invalid_native_values_are_rejected() {
    let row = observation("same-native-grant", ExecutionGrantState::Granted);
    let mut changed = row.clone();
    changed.used_kibibytes = Some("512".into());
    assert!(explain_read(&[&row, &changed], None).is_err(), "mutable counters cannot invent a second grant identity");
    let mut foreign = row.clone();
    foreign.source = "another native source".into();
    assert!(explain_read(&[&row, &foreign], None).is_err());
    assert!(explain_read(&[&row], Some(&coverage(SourceState::Empty))).is_err());
    assert!(explain_read(&[], Some(&coverage(SourceState::Collected))).is_err());
    changed.used_kibibytes = Some("-1".into());
    assert!(explain_read(&[&changed], None).is_err());
    let mut source = coverage(SourceState::Collected);
    source.source = "wrong source".into();
    assert!(explain_read(&[&row], Some(&source)).is_err());
}

#[test]
fn native_grant_order_is_deterministic_and_peak_memory_never_becomes_current_usage() {
    let first = observation("first", ExecutionGrantState::Granted);
    let mut second = observation("second", ExecutionGrantState::Granted);
    second.requested_kibibytes = Some("2048".into());
    second.maximum_used_kibibytes = Some("9007199254740993".into());
    second.used_kibibytes = None;
    let forward = explain_read(&[&first, &second], None).unwrap();
    let reverse = explain_read(&[&second, &first], None).unwrap();
    assert_eq!(forward, reverse);
    let entry = forward.entries.iter().find(|entry| entry.observation_key == second.key().unwrap()).unwrap();
    assert!(!entry.explanation.contains(&t!("view.request_memory.metric.used", n = "0")));
    assert!(!entry.explanation.contains("9007199254740993"));
}

#[test]
fn verified_resources_retain_one_exact_roster_without_turning_reservations_into_blockers_or_capacity() {
    let resource = NativeIdentity::new("native-pool").qualified("semaphore", "0");
    let mut waiter = observation("waiter", ExecutionGrantState::Waiting);
    associated(&mut waiter);
    waiter.resource = Some(resource.clone());
    waiter.queue = ExecutionGrantQueue {
        identity: Some(NativeIdentity::new("queue-1")),
        position: Some("9007199254740993".into()),
        next_candidate: Some(false),
        small: Some(true),
    };
    let mut reservation = observation("reservation", ExecutionGrantState::Granted);
    associated(&mut reservation);
    reservation.resource = Some(resource.clone());
    reservation.queue.identity = Some(NativeIdentity::new("different-queue"));
    reservation.maximum_used_kibibytes = Some("18446744073709551615".into());
    let mut other = observation("another-semaphore", ExecutionGrantState::Granted);
    associated(&mut other);
    other.resource = Some(resource.qualified("semaphore", "1"));
    let mut unverified = observation("unknown-request", ExecutionGrantState::Granted);
    unverified.resource = waiter.resource.clone();
    let explained =
        explain_read(&[&reservation, &unverified, &other, &waiter], Some(&coverage(SourceState::Partial))).unwrap();
    let group =
        explained.admission_resources.iter().find(|group| Some(&group.resource) == waiter.resource.as_ref()).unwrap();
    assert_eq!(group.participants.len(), 2);
    assert!(group.participants.iter().any(|row| row.observation_identity == waiter.identity));
    assert!(group.participants.iter().any(|row| row.observation_identity == reservation.identity));
    let subject = explained.entries.iter().find(|entry| entry.observation_identity == waiter.identity).unwrap();
    assert_eq!(subject.grant_admission, waiter.resource);
    assert!(subject.explanation.contains("9007199254740993"));
    assert!(subject.explanation.contains(&t!("view.request_memory.queue.not_next")));
    assert!(subject.explanation.contains(&t!("view.request_memory.queue.small")));
    assert!(
        explained
            .entries
            .iter()
            .find(|entry| entry.observation_identity == unverified.identity)
            .unwrap()
            .grant_admission
            .is_none()
    );
    assert!(explained.limitations.contains(&t!("view.request_memory.limit.population")));
    assert!(!serde_json::to_string(&explained).unwrap().contains("18446744073709551615"));
}

#[test]
fn missing_owner_resource_or_request_proof_keeps_native_positives_but_cannot_link_participants() {
    for missing in ["member", "epoch", "resource", "statement"] {
        let mut native = context();
        let mut row = observation("waiter", ExecutionGrantState::Waiting);
        associated(&mut row);
        row.resource = Some(NativeIdentity::new("resource"));
        match missing {
            "member" => native.partition = SourcePartition::InstanceLocal { member: None, epoch: Some("epoch".into()) },
            "epoch" => native.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "resource" => row.resource = None,
            "statement" => row.request.as_mut().unwrap().statement_identity = None,
            _ => unreachable!(),
        }
        let rows = [&row];
        let explained = super::explain(RequestMemoryEvidence { context: &native, observations: &rows }, None).unwrap();
        assert!(explained.admission_resources.is_empty(), "{missing}");
        assert!(explained.entries[0].grant_admission.is_none());
        assert!(explained.entries[0].title.contains("9007199254740993"));
        assert!(explained.entries[0].explanation.contains(&t!("view.request_memory.queue.unknown")));
    }
}

#[test]
fn admission_context_rejects_cross_database_cross_point_or_nonlocal_reads() {
    let mut row = observation("waiter", ExecutionGrantState::Waiting);
    associated(&mut row);
    for mutation in ["database", "point", "partition"] {
        let mut native = context();
        let mut changed = row.clone();
        match mutation {
            "database" => changed.request.as_mut().unwrap().database = Some("other-db".into()),
            "point" => changed.observed_at = "2020-01-01T08:01:00.001Z".into(),
            "partition" => native.partition = SourcePartition::Target,
            _ => unreachable!(),
        }
        let rows = [&changed];
        assert!(
            super::explain(RequestMemoryEvidence { context: &native, observations: &rows }, None).is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn all_native_candidate_states_remain_distinct_and_do_not_compare_queue_positions() {
    for (next, message) in [
        (Some(true), "view.request_memory.queue.next"),
        (Some(false), "view.request_memory.queue.not_next"),
        (None, "view.request_memory.queue.unknown"),
    ] {
        let mut row = observation("waiting", ExecutionGrantState::Waiting);
        row.queue.next_candidate = next;
        row.queue.position = Some("0".into());
        let explained = explain_read(&[&row], None).unwrap();
        assert!(explained.entries[0].explanation.contains(&culprit_core::i18n::t(message, &[])));
        assert!(explained.entries[0].explanation.contains(&t!("view.request_memory.queue.position", position = "0")));
    }
}
