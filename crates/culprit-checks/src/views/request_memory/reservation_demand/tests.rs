use super::*;
use crate::views::request_memory::{
    RequestMemoryEvidence, explain,
    tests::{associated, context, observation},
};
use culprit_core::{MemoryParticipantsReport, telemetry::*};

fn native_context() -> NativeReadContext {
    let mut context = context();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else { panic!() };
    member.kind = SharedResourceKind::Instance;
    context
}

fn rows(amounts: &[u64], total: u64, grantees: u64) -> Vec<ExecutionGrantObservation> {
    let mut waiter = observation("waiter", ExecutionGrantState::Waiting);
    associated(&mut waiter);
    waiter.granted_kibibytes = None;
    waiter.required_kibibytes = Some("64".into());
    waiter.requested_kibibytes = Some("1024".into());
    waiter.queue.next_candidate = Some(true);
    let mut rows = vec![waiter];
    for (index, amount) in amounts.iter().enumerate() {
        let mut grant = observation(&format!("granted-{index}"), ExecutionGrantState::Granted);
        associated(&mut grant);
        grant.granted_kibibytes = Some(amount.to_string());
        grant.maximum_used_kibibytes = Some("18446744073709551615".into());
        rows.push(grant);
    }
    for row in &mut rows {
        row.resource = Some(NativeIdentity::new("2").qualified("resource_semaphore_id", "0"));
        row.resource_observation = Some(ExecutionGrantResourceObservation {
            available_kibibytes: Some("-64".into()),
            granted_kibibytes: Some(total.to_string()),
            grantee_count: Some(grantees.to_string()),
            waiter_count: Some("1".into()),
        });
        assert!(row.is_valid());
    }
    rows
}

fn outcome(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: "current request memory".into(), state, reason: None, reason_message: None }
}

fn report(rows: &[ExecutionGrantObservation], state: SourceState) -> MemoryParticipantsReport {
    explain(
        RequestMemoryEvidence { context: &native_context(), observations: &rows.iter().collect::<Vec<_>>() },
        Some(&outcome(state)),
    )
    .unwrap()
}

fn has_demand(report: &MemoryParticipantsReport) -> bool {
    report.limitations.contains(&t!("view.request_memory.demand.limit"))
}

#[test]
fn a_large_original_reservation_is_size_context_for_the_waiter_without_a_cause_or_holder_verdict() {
    let rows = rows(&[12288], 16384, 4);
    let original = rows.clone();
    let report = report(&rows, SourceState::Collected);
    assert!(has_demand(&report));
    let waiter = &report.entries[0];
    assert!(waiter.explanation.starts_with(&t!(
        "view.request_memory.demand.larger",
        native_count = "4",
        native_reserved = "16384",
        largest = "12288"
    )));
    assert!(waiter.investigation.starts_with(&t!("view.request_memory.demand.larger_investigation")));
    assert_eq!(waiter.verify, t!("view.request_memory.resource.verify"));
    assert_eq!(waiter.mechanism, None);
    assert_eq!(waiter.qualified_request.as_ref(), rows[0].request.as_ref().map(|request| &request.key));
    assert_eq!(report.admission_resources[0].participants.len(), 2);
    assert!(!report.entries[1].explanation.contains(&t!(
        "view.request_memory.demand.larger",
        native_count = "4",
        native_reserved = "16384",
        largest = "12288"
    )));
    assert_eq!(rows, original);
    let serialized = serde_json::to_string(&report).unwrap();
    for absent in ["SELECT payload", "18446744073709551615", "causal_share", "severity", "fix_sql"] {
        assert!(!serialized.contains(absent), "{absent}");
    }
}

#[test]
fn several_smaller_captured_reservations_remain_a_lower_bound_on_the_native_population() {
    for state in [SourceState::Collected, SourceState::Partial] {
        let rows = rows(&[2048, 4096, 4096], 16384, 4);
        let report = report(&rows, state);
        assert!(has_demand(&report));
        assert!(report.entries[0].explanation.starts_with(&t!(
            "view.request_memory.demand.several",
            native_count = "4",
            native_reserved = "16384",
            n = 3,
            count = "3",
            smallest = "2048",
            largest = "4096"
        )));
        assert!(report.entries[0].investigation.starts_with(&t!("view.request_memory.demand.several_investigation")));
        assert!(report.limitations.contains(&t!("view.request_memory.limit.population")));
        assert_eq!(report.source_state, Some(state));
        if state == SourceState::Partial {
            assert!(report.limitations.contains(&t!("view.memory_participants.limit.source_partial")));
        }
    }
}

#[test]
fn half_comparison_is_exact_above_binary64_and_at_u64_max_without_summing_reservations() {
    for (amounts, total, larger) in [
        (vec![9007199254740992, 1], 18014398509481985, false),
        (vec![9007199254740993, 1], 18014398509481985, true),
        (vec![u64::MAX / 2, 1], u64::MAX, false),
        (vec![u64::MAX / 2 + 1, 1], u64::MAX, true),
    ] {
        let report = report(&rows(&amounts, total, 2), SourceState::Collected);
        assert!(has_demand(&report));
        let investigation = if larger {
            t!("view.request_memory.demand.larger_investigation")
        } else {
            t!("view.request_memory.demand.several_investigation")
        };
        assert!(report.entries[0].investigation.starts_with(&investigation));
    }
}

#[test]
fn contradicted_native_totals_or_counts_withdraw_the_comparison_without_repairing_the_raw_reservation() {
    for (amounts, total, grantees) in
        [(vec![16385], 16384, 4), (vec![4096, 4096], 16384, 1), (vec![4096], 0, 4), (vec![4096], 16384, 0)]
    {
        let rows = rows(&amounts, total, grantees);
        let original = rows.clone();
        let report = report(&rows, SourceState::Partial);
        assert!(!has_demand(&report));
        assert_eq!(report.entries.len(), rows.len());
        assert_eq!(rows, original);
    }
    for field in ["available", "total", "grantees", "waiters"] {
        let mut rows = rows(&[12288], 16384, 4);
        let native = rows[1].resource_observation.as_mut().unwrap();
        match field {
            "available" => native.available_kibibytes = Some("0".into()),
            "total" => native.granted_kibibytes = Some("16385".into()),
            "grantees" => native.grantee_count = Some("5".into()),
            "waiters" => native.waiter_count = Some("2".into()),
            _ => unreachable!(),
        }
        assert!(!has_demand(&report(&rows, SourceState::Collected)), "{field}");
    }
}

#[test]
fn zero_missing_and_healthy_observations_do_not_become_concurrent_demand_from_historical_peaks() {
    for missing in
        ["sample", "total", "grantees", "waiters", "zero-waiters", "grant", "zero-grant", "healthy", "not-next"]
    {
        let mut rows = rows(&[12288], 16384, 4);
        match missing {
            "sample" => rows[1].resource_observation = None,
            "total" => rows[0].resource_observation.as_mut().unwrap().granted_kibibytes = None,
            "grantees" => rows[0].resource_observation.as_mut().unwrap().grantee_count = None,
            "waiters" => rows[0].resource_observation.as_mut().unwrap().waiter_count = None,
            "zero-waiters" => {
                for row in &mut rows {
                    row.resource_observation.as_mut().unwrap().waiter_count = Some("0".into());
                }
            }
            "grant" => rows[1].granted_kibibytes = None,
            "zero-grant" => rows[1].granted_kibibytes = Some("0".into()),
            "healthy" => {
                for row in &mut rows {
                    row.resource_observation.as_mut().unwrap().available_kibibytes = Some("1024".into());
                }
            }
            "not-next" => rows[0].queue.next_candidate = Some(false),
            _ => unreachable!(),
        }
        let report = report(&rows, SourceState::Partial);
        assert!(!has_demand(&report), "{missing}");
        assert_eq!(report.entries.len(), 2);
    }
    assert!(!has_demand(&report(&rows(&[64], 16384, 4), SourceState::Collected)));
}

#[test]
fn unknown_owner_other_resource_or_unqualified_request_cannot_supply_size_context() {
    for missing in ["member", "epoch", "node", "resource", "statement"] {
        let mut native = native_context();
        let mut rows = rows(&[12288], 16384, 4);
        match missing {
            "member" => native.partition = SourcePartition::InstanceLocal { member: None, epoch: Some("epoch".into()) },
            "epoch" => native.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "node" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut native.partition else {
                    unreachable!()
                };
                member.kind = SharedResourceKind::Node;
            }
            "resource" => rows[1].resource = Some(NativeIdentity::new("another-resource")),
            "statement" => rows[1].request.as_mut().unwrap().statement_identity = None,
            _ => unreachable!(),
        }
        let report = explain(
            RequestMemoryEvidence { context: &native, observations: &rows.iter().collect::<Vec<_>>() },
            Some(&outcome(SourceState::Partial)),
        )
        .unwrap();
        assert!(!has_demand(&report), "{missing}");
        assert_eq!(report.entries.len(), 2);
    }
}

#[test]
fn foreign_source_database_and_native_point_cannot_be_borrowed_for_a_reservation_comparison() {
    for changed in ["source", "database", "clock", "coverage"] {
        let mut rows = rows(&[12288], 16384, 4);
        let mut source = outcome(SourceState::Collected);
        match changed {
            "source" => rows[1].source = "foreign-native-source".into(),
            "database" => rows[1].request.as_mut().unwrap().database = Some("foreign-db".into()),
            "clock" => rows[1].observed_at = "2020-01-01T08:01:00.001Z".into(),
            "coverage" => source.source = "foreign-native-source".into(),
            _ => unreachable!(),
        }
        assert!(
            explain(
                RequestMemoryEvidence { context: &native_context(), observations: &rows.iter().collect::<Vec<_>>() },
                Some(&source),
            )
            .is_err(),
            "{changed}"
        );
    }
}

#[test]
fn unreported_population_and_no_waiter_do_not_create_a_healthy_or_complete_resource_conclusion() {
    let mut rows = rows(&[12288], 16384, 4);
    let report = explain(
        RequestMemoryEvidence { context: &native_context(), observations: &rows.iter().collect::<Vec<_>>() },
        None,
    )
    .unwrap();
    assert!(!has_demand(&report));
    rows.remove(0);
    assert!(!has_demand(&self::report(&rows, SourceState::Collected)));
}

#[test]
fn output_reservation_mechanism_and_drain_advice_keep_their_existing_owner() {
    let mut rows = rows(&[12288], 16384, 4);
    rows[1].request.as_mut().unwrap().wait = Some(ActivityWait {
        class: None,
        native_type: Some("ASYNC_NETWORK_IO".into()),
        elapsed_ms: Some("3000".into()),
        owners: vec![],
        identified_resources: vec![],
        resources: None,
    });
    let report = report(&rows, SourceState::Collected);
    assert!(has_demand(&report));
    assert_eq!(
        report.entries[1].mechanism,
        Some(culprit_core::MemoryParticipantMechanism::ReservationDuringClientOutputWait)
    );
    assert_eq!(report.entries[1].investigation, t!("view.request_memory.output_wait.investigation"));
    assert_eq!(report.entries[1].verify, t!("view.request_memory.output_wait.verify"));
    assert_eq!(report.entries[0].mechanism, None);
    assert_eq!(report.entries[0].verify, t!("view.request_memory.resource.verify"));
}

#[test]
fn an_explicit_negative_same_generation_peer_withdraws_size_context_without_borrowing_its_sql() {
    let mut rows = rows(&[12288], 16384, 4);
    let mut peer = observation("negative-peer", ExecutionGrantState::Granted);
    associated(&mut peer);
    let request = peer.request.as_mut().unwrap();
    request.key.session = rows[1].request.as_ref().unwrap().key.session.clone();
    request.key.request.as_mut().unwrap().id = "1".into();
    request.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle: SessionLifecycle::Connecting,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: None,
    }));
    rows.push(peer);
    let report = report(&rows, SourceState::Partial);
    assert!(!has_demand(&report));
    let original_grant = report.entries.iter().find(|entry| entry.observation_identity == rows[1].identity).unwrap();
    assert!(original_grant.qualified_request.is_none());
    assert!(original_grant.explanation.contains(&t!("view.request_memory.request.unqualified")));
    assert_eq!(report.entries.len(), 3);
    assert!(!original_grant.explanation.contains("SELECT payload") && !original_grant.explanation.contains("9264"));
}
