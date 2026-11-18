use super::*;
use culprit_core::MemoryParticipantMechanism;

fn bound_grant(id: &str, state: ExecutionGrantState) -> ExecutionGrantObservation {
    let mut row = observation(id, state);
    associated(&mut row);
    row.resource = Some(
        NativeIdentity::new("2")
            .qualified("resource_semaphore_id", "0")
            .qualified("kind", "sql-server-resource-governor"),
    );
    if state == ExecutionGrantState::Waiting {
        row.granted_kibibytes = None;
        row.used_kibibytes = None;
    }
    row
}

fn output_wait_holder(id: &str) -> ExecutionGrantObservation {
    let mut holder = bound_grant(id, ExecutionGrantState::Granted);
    holder.request.as_mut().unwrap().wait = Some(ActivityWait {
        class: None,
        native_type: Some("ASYNC_NETWORK_IO".into()),
        elapsed_ms: Some("3000".into()),
        owners: vec![],
        identified_resources: vec![],
        resources: None,
    });
    holder
}

fn entry_for<'a>(
    report: &'a MemoryParticipantsReport,
    row: &ExecutionGrantObservation,
) -> &'a MemoryParticipantExplanation {
    report.entries.iter().find(|entry| entry.observation_identity == row.identity).unwrap()
}

#[test]
fn a_positive_reservation_and_same_request_output_wait_have_one_typed_explanation_without_blaming_a_peer() {
    let mut holder = output_wait_holder("holder");
    holder.granted_kibibytes = Some("9007199254740993".into());
    let waiter = bound_grant("waiter", ExecutionGrantState::Waiting);
    let before = [holder.clone(), waiter.clone()];
    let report = explain_read(&[&holder, &waiter], Some(&coverage(SourceState::Collected))).unwrap();
    let holder_entry = entry_for(&report, &holder);
    assert_eq!(holder_entry.mechanism, Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait));
    assert_eq!(holder_entry.qualified_request.as_ref(), holder.request.as_ref().map(|request| &request.key));
    assert!(holder_entry.title.contains("9007199254740993"));
    assert!(holder_entry.explanation.contains(&t!("view.request_memory.output_wait.explanation")));
    assert_eq!(holder_entry.investigation, t!("view.request_memory.output_wait.investigation"));
    assert_eq!(holder_entry.verify, t!("view.request_memory.output_wait.verify"));
    assert_eq!(entry_for(&report, &waiter).mechanism, None);
    assert_eq!(holder_entry.grant_admission, waiter.resource);
    assert_eq!(report.admission_resources.len(), 1);
    assert_eq!(report.admission_resources[0].participants.len(), 2);
    assert!(report.limitations.contains(&t!("view.request_memory.limit.ownership")));
    assert!(report.limitations.contains(&t!("view.request_memory.limit.population")));
    assert_eq!([holder, waiter], before, "the original wait and grant facts remain intact");
    let encoded = serde_json::to_value(&report).unwrap();
    assert!(encoded.get("grade").is_none() && encoded.get("cause").is_none() && encoded.get("recovery").is_none());
    let restored: MemoryParticipantsReport = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored, report);
}

#[test]
fn a_lone_output_wait_or_missing_resource_preserves_its_own_fact_without_inventing_queued_peers() {
    for resource_known in [true, false] {
        let mut holder = output_wait_holder("holder");
        if !resource_known {
            holder.resource = None;
        }
        let report = explain_read(&[&holder], Some(&coverage(SourceState::Collected))).unwrap();
        assert_eq!(report.entries[0].mechanism, Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait));
        assert_eq!(report.entries[0].grant_admission.is_some(), resource_known);
        assert_eq!(report.entries[0].qualified_request.as_ref(), holder.request.as_ref().map(|request| &request.key));
        assert_eq!(report.admission_resources.len(), usize::from(resource_known));
        assert!(report.admission_resources.iter().all(|resource| resource.participants.len() == 1));
        assert!(report.limitations.contains(&t!("view.request_memory.limit.population")));
    }
}

#[test]
fn another_wait_or_unmeasured_reservation_does_not_acquire_the_output_wait_mechanism() {
    for native_wait in ["LCK_M_X", "RESOURCE_SEMAPHORE", "NET_WAIT", "ASYNC_NETWORK_IO_FUTURE", "ClientWrite"] {
        let mut holder = output_wait_holder("holder");
        holder.request.as_mut().unwrap().wait.as_mut().unwrap().native_type = Some(native_wait.into());
        let report = explain_read(&[&holder], None).unwrap();
        assert_eq!(report.entries[0].mechanism, None, "{native_wait}");
        assert_eq!(report.entries[0].investigation, t!("view.request_memory.granted.investigation"));
    }
    for granted in [None, Some("0")] {
        let mut holder = output_wait_holder("holder");
        holder.granted_kibibytes = granted.map(str::to_owned);
        let report = explain_read(&[&holder], None).unwrap();
        assert_eq!(report.entries[0].mechanism, None);
    }
    let mut waiter = output_wait_holder("waiter");
    waiter.state = ExecutionGrantState::Waiting;
    waiter.granted_without_offset = None;
    waiter.granted_kibibytes = None;
    let report = explain_read(&[&waiter], None).unwrap();
    assert_eq!(report.entries[0].mechanism, None);
    let mut holder = output_wait_holder("holder");
    holder.request.as_mut().unwrap().wait = None;
    assert_eq!(explain_read(&[&holder], None).unwrap().entries[0].mechanism, None);
}

#[test]
fn missing_request_binding_or_native_scope_withholds_the_mechanism_without_inventing_a_zero_reservation() {
    for association in [
        RequestGrantAssociation::MissingRequest,
        RequestGrantAssociation::ChangedRequest,
        RequestGrantAssociation::InsufficientEvidence,
    ] {
        let mut holder = output_wait_holder("holder");
        holder.request = None;
        holder.scope = None;
        holder.request_association = association;
        holder.statement_relationship = GrantStatementRelationship::Unresolved;
        let report = explain_read(&[&holder], None).unwrap();
        assert_eq!(report.entries[0].mechanism, None);
        assert!(report.entries[0].title.contains("1024"));
    }
    for missing in ["member", "epoch", "statement"] {
        let mut native_context = context();
        let mut holder = output_wait_holder("holder");
        match missing {
            "member" => {
                native_context.partition = SourcePartition::InstanceLocal { member: None, epoch: Some("epoch".into()) };
            }
            "epoch" => native_context.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "statement" => holder.request.as_mut().unwrap().statement_identity = None,
            _ => unreachable!(),
        }
        let rows = [&holder];
        let report =
            super::super::explain(RequestMemoryEvidence { context: &native_context, observations: &rows }, None)
                .unwrap();
        assert_eq!(report.entries[0].mechanism, None, "{missing}");
        assert_eq!(report.entries[0].qualified_request, None, "{missing}");
        assert!(report.entries[0].explanation.contains(&t!("view.request_memory.request.unqualified")));
        assert!(report.admission_resources.is_empty(), "{missing}");
    }
}

#[test]
fn restricted_sql_and_partial_read_qualification_do_not_erase_a_known_native_reservation_and_wait() {
    let mut holder = output_wait_holder("holder");
    let request = holder.request.as_mut().unwrap();
    request.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    request.client = Some(ActivityClient { application: Some("private-client-canary".into()), user: None, host: None });
    holder.statement_relationship = GrantStatementRelationship::Unresolved;
    let report = explain_read(&[&holder], Some(&coverage(SourceState::Partial))).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.entries[0].qualified_request.as_ref(), holder.request.as_ref().map(|request| &request.key));
    assert_eq!(report.entries[0].mechanism, Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait));
    assert!(report.entries[0].explanation.contains(&t!("view.request_memory.request.statement_unavailable")));
    assert!(report.limitations.contains(&t!("view.memory_participants.limit.source_partial")));
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(
        !encoded.contains("SELECT payload") && !encoded.contains("9264") && !encoded.contains("private-client-canary")
    );
}

#[test]
fn negative_mars_peers_outside_the_resource_group_withdraw_only_their_exact_generation() {
    for (lifecycle, eligible) in [
        (SessionLifecycle::Running, true),
        (SessionLifecycle::Unknown, true),
        (SessionLifecycle::Connecting, false),
        (SessionLifecycle::PoolResetPrelogin, false),
    ] {
        let holder = output_wait_holder("holder");
        let mut peer = bound_grant("peer", ExecutionGrantState::Granted);
        let peer_request = peer.request.as_mut().unwrap();
        peer_request.key.session = holder.request.as_ref().unwrap().key.session.clone();
        peer_request.key.request.as_mut().unwrap().id = "1".into();
        peer_request.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        peer.resource = None;
        let waiter = bound_grant("waiter", ExecutionGrantState::Waiting);
        let report = explain_read(&[&holder, &peer, &waiter], Some(&coverage(SourceState::Partial))).unwrap();
        let holder_entry = entry_for(&report, &holder);
        assert_eq!(holder_entry.mechanism.is_some(), eligible, "{lifecycle:?}");
        assert_eq!(holder_entry.grant_admission.is_some(), eligible, "{lifecycle:?}");
        assert_eq!(holder_entry.qualified_request.is_some(), eligible, "{lifecycle:?}");
        if !eligible {
            assert!(holder_entry.explanation.contains(&t!("view.request_memory.request.unqualified")));
            assert!(!holder_entry.explanation.contains(&t!("view.request_memory.request.observed_statement")));
        }
        assert!(entry_for(&report, &waiter).grant_admission.is_some(), "an independent generation stays qualified");
        assert_eq!(report.entries.len(), 3, "raw native grant points remain represented");
        assert_eq!(report.admission_resources[0].participants.len(), if eligible { 2 } else { 1 });
    }
}

#[test]
fn duplicate_changed_and_malformed_embedded_requests_cannot_be_filtered_out_before_population_validation() {
    let holder = output_wait_holder("holder");
    let mut peer = bound_grant("peer", ExecutionGrantState::Granted);
    peer.request.as_mut().unwrap().key = holder.request.as_ref().unwrap().key.clone();
    assert!(explain_read(&[&holder, &peer], None).is_err());
    peer.request.as_mut().unwrap().key.session.generation = "a-new-login".into();
    assert!(explain_read(&[&holder, &peer], None).is_err());
    let mut malformed = bound_grant("unrelated", ExecutionGrantState::Granted);
    malformed.resource = None;
    malformed.request.as_mut().unwrap().key.session.id.clear();
    assert!(explain_read(&[&holder, &malformed], None).is_err());
}

#[test]
fn another_native_resource_cannot_become_a_waiting_peer_of_the_output_wait_holder() {
    let holder = output_wait_holder("holder");
    let mut waiter = bound_grant("waiter", ExecutionGrantState::Waiting);
    waiter.resource.as_mut().unwrap().qualifiers.insert("resource_semaphore_id".into(), "1".into());
    assert!(!holder.shares_admission_resource(&waiter, "db"));
    let report = explain_read(&[&holder, &waiter], None).unwrap();
    assert_eq!(report.admission_resources.len(), 2);
    assert!(report.admission_resources.iter().all(|resource| resource.participants.len() == 1));
    assert_ne!(entry_for(&report, &holder).grant_admission, entry_for(&report, &waiter).grant_admission);
    assert_eq!(
        entry_for(&report, &holder).mechanism,
        Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait)
    );
    assert_eq!(entry_for(&report, &waiter).mechanism, None);
}

#[test]
fn the_same_native_spelling_cannot_borrow_another_engines_wait_meaning() {
    for engine in culprit_core::Engine::ALL.into_iter().filter(|engine| *engine != culprit_core::Engine::Mssql) {
        let mut native_context = context();
        let CollectionTarget::Database(database) = &mut native_context.target else { panic!() };
        database.engine = engine;
        let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut native_context.partition else {
            panic!()
        };
        member.engine = engine;
        let holder = output_wait_holder("holder");
        let rows = [&holder];
        let report =
            super::super::explain(RequestMemoryEvidence { context: &native_context, observations: &rows }, None)
                .unwrap();
        assert_eq!(report.entries[0].mechanism, None, "{engine:?}");
    }
}
