use super::*;

fn waiter(available: Option<&str>) -> ExecutionGrantObservation {
    let mut row = observation("waiter", ExecutionGrantState::Waiting);
    associated(&mut row);
    row.resource = Some(NativeIdentity::new("2").qualified("resource_semaphore_id", "0"));
    row.granted_kibibytes = None;
    row.required_kibibytes = Some("64".into());
    row.requested_kibibytes = Some("1024".into());
    row.queue.next_candidate = Some(true);
    row.resource_observation = Some(ExecutionGrantResourceObservation {
        available_kibibytes: available.map(str::to_owned),
        granted_kibibytes: Some("9007199254740993".into()),
        grantee_count: Some("2".into()),
        waiter_count: Some("1".into()),
    });
    assert!(row.is_valid());
    row
}

fn admission_context() -> NativeReadContext {
    let mut native = context();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut native.partition else {
        panic!("native test owner")
    };
    member.kind = SharedResourceKind::Instance;
    native
}

fn explain_resource_read(
    rows: &[&ExecutionGrantObservation],
    source: Option<&SourceOutcome>,
) -> Result<MemoryParticipantsReport> {
    super::super::explain(RequestMemoryEvidence { context: &admission_context(), observations: rows }, source)
}

fn entry<'a>(
    report: &'a MemoryParticipantsReport,
    row: &ExecutionGrantObservation,
) -> &'a culprit_core::MemoryParticipantExplanation {
    report.entries.iter().find(|entry| entry.observation_identity == row.identity).unwrap()
}

#[test]
fn native_available_memory_distinguishes_required_and_requested_without_an_admission_or_cause_verdict() {
    for available in ["-64", "0", "63", "64", "1000", "1023"] {
        let row = waiter(Some(available));
        let original = row.clone();
        let report = explain_resource_read(&[&row], Some(&coverage(SourceState::Collected))).unwrap();
        let explanation = entry(&report, &row);
        let detail = if available.parse::<i64>().unwrap() < 64 {
            t!(
                "view.request_memory.resource.below_required",
                available = available,
                required = "64",
                requested = "1024"
            )
        } else {
            t!(
                "view.request_memory.resource.below_requested",
                available = available,
                required = "64",
                requested = "1024"
            )
        };
        assert!(explanation.explanation.contains(&detail), "{available}");
        assert!(explanation.explanation.contains(&t!("view.request_memory.resource.limit")));
        assert_eq!(explanation.investigation, t!("view.request_memory.resource.investigation"));
        assert_eq!(explanation.verify, t!("view.request_memory.resource.verify"));
        assert_eq!(explanation.mechanism, None);
        assert_eq!(row, original);
        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("severity").is_none() && json.get("capacity").is_none() && json.get("cause").is_none());
    }
}

#[test]
fn native_bounds_are_compared_as_exact_integers_beyond_binary64_and_without_capacity_sums() {
    let mut row = waiter(Some("9007199254740992"));
    row.required_kibibytes = Some("9007199254740993".into());
    row.requested_kibibytes = Some("9007199254740994".into());
    let report = explain_resource_read(&[&row], None).unwrap();
    assert!(report.entries[0].explanation.contains(&t!(
        "view.request_memory.resource.below_required",
        available = "9007199254740992",
        required = "9007199254740993",
        requested = "9007199254740994"
    )));
    assert!(report.limitations.contains(&t!("view.request_memory.limit.population")));
    row.resource_observation.as_mut().unwrap().available_kibibytes = Some("9007199254740993".into());
    let report = explain_resource_read(&[&row], None).unwrap();
    assert!(report.entries[0].explanation.contains(&t!(
        "view.request_memory.resource.below_requested",
        available = "9007199254740993",
        required = "9007199254740993",
        requested = "9007199254740994"
    )));
}

#[test]
fn missing_zero_inconsistent_or_satisfied_bounds_do_not_establish_a_waiting_resource_shortfall() {
    for change in [
        "sample",
        "available",
        "required",
        "requested",
        "zero-required",
        "inconsistent",
        "equal",
        "above",
        "not-next",
        "next-unknown",
        "granted",
        "positive-waiting-grant",
    ] {
        let mut row = waiter(Some("0"));
        match change {
            "sample" => row.resource_observation = None,
            "available" => row.resource_observation.as_mut().unwrap().available_kibibytes = None,
            "required" => row.required_kibibytes = None,
            "requested" => row.requested_kibibytes = None,
            "zero-required" => row.required_kibibytes = Some("0".into()),
            "inconsistent" => row.required_kibibytes = Some("1025".into()),
            "equal" => row.resource_observation.as_mut().unwrap().available_kibibytes = Some("1024".into()),
            "above" => row.resource_observation.as_mut().unwrap().available_kibibytes = Some("1025".into()),
            "not-next" => row.queue.next_candidate = Some(false),
            "next-unknown" => row.queue.next_candidate = None,
            "granted" => {
                row.state = ExecutionGrantState::Granted;
                row.granted_without_offset = Some("2020-01-01T10:00:00.006".into());
                row.granted_kibibytes = Some("1024".into());
            }
            "positive-waiting-grant" => row.granted_kibibytes = Some("1".into()),
            _ => unreachable!(),
        }
        assert!(row.is_valid(), "{change}: raw values remain available");
        let report = explain_resource_read(&[&row], None).unwrap();
        assert!(!report.entries[0].explanation.contains(&t!("view.request_memory.resource.limit")), "{change}");
        assert_ne!(report.entries[0].investigation, t!("view.request_memory.resource.investigation"), "{change}");
    }
}

#[test]
fn partial_or_unknown_populations_keep_a_positive_resource_condition_and_withhold_sql_ownership() {
    let mut row = waiter(Some("0"));
    row.request.as_mut().unwrap().statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    row.statement_relationship = GrantStatementRelationship::Unresolved;
    for source in [Some(coverage(SourceState::Partial)), None] {
        let report = explain_resource_read(&[&row], source.as_ref()).unwrap();
        assert!(report.entries[0].explanation.contains(&t!("view.request_memory.resource.limit")));
        assert_eq!(report.entries[0].qualified_request.as_ref(), row.request.as_ref().map(|request| &request.key));
        assert!(report.limitations.contains(&t!("view.request_memory.limit.population")));
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("SELECT payload") && !encoded.contains("9264"));
    }
}

#[test]
fn resource_values_cannot_repair_missing_owner_statement_or_request_authority() {
    for missing in ["member", "epoch", "statement", "request"] {
        let mut native = admission_context();
        let mut row = waiter(Some("0"));
        match missing {
            "member" => native.partition = SourcePartition::InstanceLocal { member: None, epoch: Some("epoch".into()) },
            "epoch" => native.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "statement" => row.request.as_mut().unwrap().statement_identity = None,
            "request" => {
                row.request = None;
                row.scope = None;
                row.request_association = RequestGrantAssociation::ChangedRequest;
                row.statement_relationship = GrantStatementRelationship::Unresolved;
            }
            _ => unreachable!(),
        }
        let rows = [&row];
        let report =
            super::super::explain(RequestMemoryEvidence { context: &native, observations: &rows }, None).unwrap();
        assert!(!report.entries[0].explanation.contains(&t!("view.request_memory.resource.limit")), "{missing}");
        assert!(report.entries[0].qualified_request.is_none());
        assert!(row.resource_observation.is_some(), "native positives are retained");
    }
}

#[test]
fn negative_mars_peers_withdraw_the_relation_only_for_their_observed_session_generation() {
    for same_generation in [true, false] {
        let row = waiter(Some("0"));
        let mut peer = waiter(Some("0"));
        peer.identity.id = "mars-peer".into();
        peer.resource = Some(NativeIdentity::new("another-resource"));
        let request = peer.request.as_mut().unwrap();
        request.key.request.as_mut().unwrap().id = "1".into();
        if !same_generation {
            request.key.session.id = "independent-session".into();
            request.key.session.generation = "another-login".into();
        }
        request.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle: SessionLifecycle::PoolResetPrelogin,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        if !same_generation {
            let mut recycled = peer.clone();
            recycled.request.as_mut().unwrap().key.session.id = row.request.as_ref().unwrap().key.session.id.clone();
            assert_eq!(
                explain_resource_read(&[&recycled, &row], None).unwrap_err().detail,
                Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)),
                "two generations of the same native session cannot coexist in one frame"
            );
        }
        let report = explain_resource_read(&[&peer, &row], None).unwrap();
        assert_eq!(
            entry(&report, &row).explanation.contains(&t!("view.request_memory.resource.limit")),
            !same_generation
        );
    }
}

#[test]
fn independent_resource_samples_and_existing_output_reservations_keep_their_own_mechanisms() {
    let row = waiter(Some("0"));
    let mut holder = super::output_wait::output_wait_holder("holder");
    holder.resource = row.resource.clone();
    holder.resource_observation = Some(ExecutionGrantResourceObservation {
        available_kibibytes: Some("2048".into()),
        granted_kibibytes: Some("1024".into()),
        grantee_count: Some("2".into()),
        waiter_count: Some("1".into()),
    });
    for rows in [[&row, &holder], [&holder, &row]] {
        let report = explain_resource_read(&rows, None).unwrap();
        let waiting = entry(&report, &row);
        let granted = entry(&report, &holder);
        assert!(waiting.explanation.contains(&t!("view.request_memory.resource.limit")));
        assert_eq!(
            granted.mechanism,
            Some(culprit_core::MemoryParticipantMechanism::ReservationDuringClientOutputWait)
        );
        assert_eq!(granted.investigation, t!("view.request_memory.output_wait.investigation"));
        assert!(!granted.explanation.contains(&t!("view.request_memory.resource.limit")));
        assert_eq!(report.admission_resources.len(), 1);
    }
}

#[test]
fn tenant_node_scope_and_zero_identifiers_do_not_upgrade_a_shared_resource_observation() {
    let mut row = waiter(Some("0"));
    row.resource = Some(NativeIdentity::new("0").qualified("resource_semaphore_id", "0"));
    let native = context();
    let rows = [&row];
    let report = super::super::explain(RequestMemoryEvidence { context: &native, observations: &rows }, None).unwrap();
    assert!(report.entries[0].qualified_request.is_some(), "original request context is preserved");
    assert!(!report.entries[0].explanation.contains(&t!("view.request_memory.resource.limit")));
    assert!(report.limitations.contains(&t!("view.request_memory.resource.unqualified")));
    assert!(row.resource_observation.is_some(), "raw values cannot acquire stronger ownership");
}
