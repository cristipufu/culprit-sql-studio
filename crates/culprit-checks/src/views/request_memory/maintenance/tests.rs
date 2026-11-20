use super::*;
use crate::views::request_memory::{
    RequestMemoryEvidence, explain,
    tests::{associated, context, observation},
};
use culprit_core::{
    MemoryParticipantExplanation, MemoryParticipantMechanism, MemoryParticipantsReport, SourceOutcome, SourceState,
    telemetry::*,
};

const REBUILD: &str = "ALTER INDEX ix_orders_customer ON dbo.orders REBUILD";

fn native_context() -> NativeReadContext {
    let mut native = context();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut native.partition else { panic!() };
    member.kind = SharedResourceKind::Instance;
    native
}

fn rows() -> Vec<ExecutionGrantObservation> {
    let mut waiter = observation("waiter", ExecutionGrantState::Waiting);
    associated(&mut waiter);
    waiter.granted_kibibytes = None;
    waiter.used_kibibytes = None;
    waiter.required_kibibytes = Some("64".into());
    waiter.requested_kibibytes = Some("1024".into());
    waiter.queue.next_candidate = Some(true);
    let mut rebuild = observation("rebuild", ExecutionGrantState::Granted);
    associated(&mut rebuild);
    rebuild.request.as_mut().unwrap().statement = DefinitionPayload::sql(REBUILD, Engine::Mssql);
    let mut rows = vec![waiter, rebuild];
    for row in &mut rows {
        row.resource = Some(NativeIdentity::new("2").qualified("resource_semaphore_id", "0"));
        row.resource_observation = Some(ExecutionGrantResourceObservation {
            available_kibibytes: Some("0".into()),
            granted_kibibytes: Some("8192".into()),
            grantee_count: Some("4".into()),
            waiter_count: Some("1".into()),
        });
        assert!(row.is_valid());
    }
    rows
}

fn report(rows: &[ExecutionGrantObservation], state: SourceState) -> MemoryParticipantsReport {
    explain(
        RequestMemoryEvidence { context: &native_context(), observations: &rows.iter().collect::<Vec<_>>() },
        Some(&SourceOutcome { source: "current request memory".into(), state, reason: None, reason_message: None }),
    )
    .unwrap()
}

fn entry<'a>(
    report: &'a MemoryParticipantsReport,
    row: &ExecutionGrantObservation,
) -> &'a MemoryParticipantExplanation {
    report.entries.iter().find(|entry| entry.observation_identity == row.identity).unwrap()
}

#[test]
fn a_rebuild_reservation_is_original_same_resource_context_without_a_cause_or_allocating_statement_verdict() {
    let rows = rows();
    let original = rows.clone();
    let report = report(&rows, SourceState::Collected);
    let waiter = entry(&report, &rows[0]);
    let rebuild = entry(&report, &rows[1]);
    assert!(waiter.explanation.contains(&t!("view.request_memory.maintenance.same_resource")));
    assert!(!waiter.explanation.contains(&t!("view.request_memory.maintenance.reserved")));
    assert!(rebuild.explanation.contains(&t!("view.request_memory.maintenance.reserved")));
    assert!(rebuild.explanation.contains(&t!("view.request_memory.request.observed_statement")));
    for participant in [waiter, rebuild] {
        assert!(participant.investigation.ends_with(&t!("view.request_memory.maintenance.investigate")));
        assert!(participant.verify.ends_with(&t!("view.request_memory.maintenance.verify")));
        assert_eq!(participant.mechanism, None);
    }
    assert_eq!(waiter.grant_admission, rebuild.grant_admission);
    assert_eq!(report.admission_resources[0].participants.len(), 2);
    assert!(report.limitations.contains(&t!("view.request_memory.limit.ownership")));
    assert_eq!(rows, original);
    let json = serde_json::to_string(&report).unwrap();
    for absent in [REBUILD, "SELECT payload", "9264", "causal_share", "severity", "fix_sql"] {
        assert!(!json.contains(absent), "{absent}");
    }
}

#[test]
fn a_rebuild_waiting_for_memory_is_affected_and_never_supplies_a_reserved_peer() {
    let mut rows = rows();
    rows[0].request.as_mut().unwrap().statement = DefinitionPayload::sql(REBUILD, Engine::Mssql);
    rows[1].request.as_mut().unwrap().statement = DefinitionPayload::sql("SELECT 1", Engine::Mssql);
    let report = report(&rows, SourceState::Collected);
    assert!(entry(&report, &rows[0]).explanation.contains(&t!("view.request_memory.maintenance.waiting")));
    for entry in &report.entries {
        assert!(!entry.explanation.contains(&t!("view.request_memory.maintenance.reserved")));
        assert!(!entry.explanation.contains(&t!("view.request_memory.maintenance.same_resource")));
    }
    assert_eq!(report.entries[0].mechanism, None);
    assert!(report.limitations.contains(&t!("view.request_memory.limit.ownership")));
}

#[test]
fn healthy_maintenance_and_unmeasured_or_zero_reservations_do_not_create_a_queue_contributor() {
    let mut healthy_rows = rows();
    for row in &mut healthy_rows {
        let sample = row.resource_observation.as_mut().unwrap();
        sample.available_kibibytes = Some("8192".into());
        sample.waiter_count = Some("0".into());
    }
    let healthy = report(&healthy_rows[1..], SourceState::Collected);
    assert!(healthy.entries[0].explanation.contains(&t!("view.request_memory.maintenance.reserved")));
    assert!(!healthy.entries[0].explanation.contains(&t!("view.request_memory.maintenance.same_resource")));
    assert!(!healthy.entries[0].explanation.contains(&t!("view.request_memory.resource.limit")));
    for grant in [None, Some("0")] {
        let mut rows = rows();
        rows[1].granted_kibibytes = grant.map(str::to_owned);
        let report = report(&rows, SourceState::Collected);
        assert!(
            report
                .entries
                .iter()
                .all(|entry| !entry.explanation.contains(&t!("view.request_memory.maintenance.reserved")))
        );
        assert!(
            report
                .entries
                .iter()
                .all(|entry| !entry.explanation.contains(&t!("view.request_memory.maintenance.same_resource")))
        );
    }
}

#[test]
fn partial_visibility_retains_observed_roles_without_claiming_a_complete_population() {
    for state in [SourceState::Collected, SourceState::Partial] {
        let rows = rows();
        let report = report(&rows, state);
        assert!(entry(&report, &rows[0]).explanation.contains(&t!("view.request_memory.maintenance.same_resource")));
        assert_eq!(report.source_state, Some(state));
        assert!(report.limitations.contains(&t!("view.request_memory.limit.population")));
        if state == SourceState::Partial {
            assert!(report.limitations.contains(&t!("view.memory_participants.limit.source_partial")));
        }
    }
}

#[test]
fn another_resource_or_missing_shared_owner_cannot_lend_maintenance_context_to_a_waiter() {
    let mut foreign = rows();
    foreign[1].resource.as_mut().unwrap().qualifiers.insert("resource_semaphore_id".into(), "1".into());
    let report = report(&foreign, SourceState::Collected);
    assert!(!entry(&report, &foreign[0]).explanation.contains(&t!("view.request_memory.maintenance.same_resource")));
    assert!(entry(&report, &foreign[1]).explanation.contains(&t!("view.request_memory.maintenance.reserved")));
    for missing in ["member", "epoch", "instance", "resource"] {
        let mut native = native_context();
        let mut rows = rows();
        match missing {
            "member" => native.partition = SourcePartition::InstanceLocal { member: None, epoch: Some("epoch".into()) },
            "epoch" => native.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "instance" => native = context(),
            "resource" => {
                rows[1].resource = None;
                rows[1].resource_observation = None;
            }
            _ => unreachable!(),
        }
        let report =
            explain(RequestMemoryEvidence { context: &native, observations: &rows.iter().collect::<Vec<_>>() }, None)
                .unwrap();
        assert!(
            report
                .entries
                .iter()
                .all(|entry| !entry.explanation.contains(&t!("view.request_memory.maintenance.reserved"))),
            "{missing}"
        );
        assert!(
            !entry(&report, &rows[0]).explanation.contains(&t!("view.request_memory.maintenance.same_resource")),
            "{missing}"
        );
    }
}

#[test]
fn unavailable_changed_or_command_only_statements_cannot_classify_a_reserved_rebuild() {
    for missing in ["payload", "relationship", "statement", "changed", "command", "multiple", "other-maintenance"] {
        let mut rows = rows();
        let rebuild = &mut rows[1];
        match missing {
            "payload" => {
                rebuild.request.as_mut().unwrap().statement =
                    DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
            }
            "relationship" => rebuild.statement_relationship = GrantStatementRelationship::Unresolved,
            "statement" => rebuild.request.as_mut().unwrap().statement_identity = None,
            "changed" => {
                rebuild.request = None;
                rebuild.scope = None;
                rebuild.request_association = RequestGrantAssociation::ChangedRequest;
                rebuild.statement_relationship = GrantStatementRelationship::Unresolved;
            }
            "command" => {
                let request = rebuild.request.as_mut().unwrap();
                request.command = Some("ALTER INDEX".into());
                request.statement = DefinitionPayload::sql("SELECT 1", Engine::Mssql);
            }
            "multiple" => {
                rebuild.request.as_mut().unwrap().statement =
                    DefinitionPayload::sql(&format!("{REBUILD}; SELECT 1"), Engine::Mssql)
            }
            "other-maintenance" => {
                rebuild.request.as_mut().unwrap().statement =
                    DefinitionPayload::sql("UPDATE STATISTICS dbo.orders", Engine::Mssql)
            }
            _ => unreachable!(),
        }
        assert!(rebuild.is_valid(), "{missing}");
        let report = report(&rows, SourceState::Partial);
        for entry in &report.entries {
            assert!(!entry.explanation.contains(&t!("view.request_memory.maintenance.reserved")), "{missing}");
            assert!(!entry.explanation.contains(&t!("view.request_memory.maintenance.same_resource")), "{missing}");
        }
    }
}

#[test]
fn native_scope_clock_and_negative_generation_evidence_cannot_be_repaired_by_maintenance_syntax() {
    for changed in ["clock", "source", "database", "duplicate-request"] {
        let mut rows = rows();
        match changed {
            "clock" => rows[1].observed_at = "2020-01-01T08:01:01Z".into(),
            "source" => rows[1].source = "foreign-source".into(),
            "database" => rows[1].request.as_mut().unwrap().database = Some("foreign-db".into()),
            "duplicate-request" => {
                rows[1].request.as_mut().unwrap().key = rows[0].request.as_ref().unwrap().key.clone()
            }
            _ => unreachable!(),
        }
        assert!(
            explain(
                RequestMemoryEvidence { context: &native_context(), observations: &rows.iter().collect::<Vec<_>>() },
                None
            )
            .is_err(),
            "{changed}"
        );
    }
    let mut rows = rows();
    let mut peer = rows[1].clone();
    peer.identity.id = "negative-peer".into();
    let request = peer.request.as_mut().unwrap();
    request.key.request.as_mut().unwrap().id = "1".into();
    request.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle: SessionLifecycle::PoolResetPrelogin,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: None,
    }));
    rows.push(peer);
    let report = report(&rows, SourceState::Partial);
    assert!(!entry(&report, &rows[0]).explanation.contains(&t!("view.request_memory.maintenance.same_resource")));
    assert!(entry(&report, &rows[1]).qualified_request.is_none());
    assert!(entry(&report, &rows[1]).explanation.contains(&t!("view.request_memory.request.unqualified")));
    assert_eq!(report.entries.len(), 3);
}

#[test]
fn an_observed_output_wait_keeps_its_existing_mechanism_and_advice_without_a_second_maintenance_case() {
    let mut rows = rows();
    rows[1].request.as_mut().unwrap().wait = Some(ActivityWait {
        class: None,
        native_type: Some("ASYNC_NETWORK_IO".into()),
        elapsed_ms: Some("3000".into()),
        owners: vec![],
        identified_resources: vec![],
        resources: None,
    });
    let report = report(&rows, SourceState::Collected);
    let output = entry(&report, &rows[1]);
    assert_eq!(output.mechanism, Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait));
    assert_eq!(output.investigation, t!("view.request_memory.output_wait.investigation"));
    assert_eq!(output.verify, t!("view.request_memory.output_wait.verify"));
    assert!(!output.explanation.contains(&t!("view.request_memory.maintenance.reserved")));
    assert!(!entry(&report, &rows[0]).explanation.contains(&t!("view.request_memory.maintenance.same_resource")));
}
