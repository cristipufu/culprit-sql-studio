use super::*;
use crate::views::request_memory::tests::{associated, context, observation};
use culprit_core::{Check, CheckStatus, OpenTransaction, SourceOutcome, TransactionActivity};
use serde_json::json;

fn grant(id: &str, state: ExecutionGrantState) -> ExecutionGrantObservation {
    let mut row = observation(id, state);
    associated(&mut row);
    row.resource = Some(NativeIdentity::new("pool-2").qualified("resource_semaphore_id", "0"));
    if state == ExecutionGrantState::Waiting {
        row.granted_kibibytes = None;
        row.used_kibibytes = None;
    }
    let request = row.request.as_mut().unwrap();
    request.statement = DefinitionPayload::sql(
        if state == ExecutionGrantState::Waiting {
            "SELECT waiting_marker FROM dbo.orders"
        } else {
            "SELECT output_marker FROM dbo.large_rows"
        },
        culprit_core::Engine::Mssql,
    );
    if state == ExecutionGrantState::Granted {
        request.wait = Some(ActivityWait {
            native_type: Some("ASYNC_NETWORK_IO".into()),
            elapsed_ms: Some("3000".into()),
            class: None,
            owners: vec![],
            identified_resources: vec![],
            resources: None,
        });
    }
    assert!(row.is_valid());
    row
}
fn snapshot(rows: Vec<ExecutionGrantObservation>) -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let context = context();
    snapshot.server.database = "db".into();
    snapshot.identity = context.target.database().cloned();
    snapshot.workload = None;
    snapshot.waits.clear();
    let pressure = snapshot.pressure.get_or_insert_default();
    pressure.signals.clear();
    pressure.request_memory = Some(RequestMemorySnapshot {
        source: "current request memory".into(),
        observed_at: context.observed_from.clone(),
        context,
        observations: rows,
    });
    snapshot.sources.retain(|source| source.source != "current request memory");
    snapshot.sources.push(SourceOutcome {
        source: "current request memory".into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    snapshot
}
fn partial_snapshot(rows: Vec<ExecutionGrantObservation>) -> Snapshot {
    let mut snapshot = snapshot(rows);
    snapshot.sources.last_mut().unwrap().state = SourceState::Partial;
    snapshot
}
fn unassociated(mut row: ExecutionGrantObservation) -> ExecutionGrantObservation {
    row.request = None;
    row.scope = None;
    row.request_association = RequestGrantAssociation::MissingRequest;
    row.statement_relationship = GrantStatementRelationship::Unresolved;
    assert!(row.is_valid());
    row
}
fn captured(snapshot: &mut Snapshot) -> &mut RequestMemorySnapshot {
    snapshot.pressure.as_mut().unwrap().request_memory.as_mut().unwrap()
}
fn finding_at_point(snapshot: &Snapshot) -> Option<Finding> {
    decide(snapshot).map(render)
}
fn observed_text(finding: &Finding) -> String {
    serde_json::to_string(&finding.metrics).unwrap()
}

#[test]
fn native_waiting_sql_and_output_reservation_have_an_independent_informational_diagnosis() {
    let snapshot = snapshot(vec![grant("41", ExecutionGrantState::Waiting), grant("42", ExecutionGrantState::Granted)]);
    let finding = finding_at_point(&snapshot).unwrap();
    assert_eq!(finding.check, "memory-queue");
    assert_eq!(finding.actionability, Actionability::Information);
    assert_eq!(finding.severity, Severity::Low);
    assert!(finding.summary.contains("2020-01-01T08:01:00Z"));
    assert!(observed_text(&finding).contains("waiting_marker"));
    assert!(observed_text(&finding).contains("output_marker"));
    assert_eq!(finding.fix.as_ref().unwrap().summary, t!("view.request_memory.output_wait.investigation"));
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
    assert!(finding.queries.is_empty(), "native request SQL cannot invent a workload query reference");
    assert_eq!(finding.verify, vec![t!("view.request_memory.output_wait.verify")]);
}

#[test]
fn partial_native_reads_preserve_positive_requests_as_a_lower_bound_without_counting_unowned_rows() {
    for count in [1_usize, 2] {
        let mut rows: Vec<_> =
            (41..41 + count).map(|id| grant(&id.to_string(), ExecutionGrantState::Waiting)).collect();
        rows.push(grant("51", ExecutionGrantState::Granted));
        for id in 81..85 {
            let mut unknown = unassociated(grant(&id.to_string(), ExecutionGrantState::Waiting));
            unknown.queue.position = Some("9007199254740997".into());
            rows.push(unknown);
        }
        let collected = finding_at_point(&snapshot(rows.clone())).unwrap();
        let snapshot = partial_snapshot(rows);
        let case = decide(&snapshot).expect("partial visibility does not erase independently qualified requests");
        assert_eq!(case.source_state, SourceState::Partial);
        assert_eq!(case.waiting.len(), count);
        assert_eq!(case.reservations.len(), 1);
        let finding = render(case);
        assert_eq!(finding.id, collected.id, "source completeness cannot invent a different native point");
        let expected_summary = match count {
            1 => "Recorded at 2020-01-01T08:01:00Z. At least 1 request was waiting for query memory.",
            2 => "Recorded at 2020-01-01T08:01:00Z. At least 2 requests were waiting for query memory.",
            _ => unreachable!(),
        };
        assert_eq!(finding.summary, format!("{expected_summary} {}", t!("check.memory_queue.recorded.output_context")));
        assert_eq!(
            collected.summary,
            format!(
                "Recorded at 2020-01-01T08:01:00Z. Requests waiting for query memory: {count}. {}",
                t!("check.memory_queue.recorded.output_context")
            ),
            "the collected source's words stay unchanged"
        );
        assert!(finding.caveats.contains(&t!("view.memory_participants.limit.source_partial")));
        assert!(!collected.caveats.contains(&t!("view.memory_participants.limit.source_partial")));
        assert_eq!(finding.actionability, Actionability::Information);
        assert_eq!(finding.severity, Severity::Low);
        assert_eq!(
            finding.metrics.iter().filter(|metric| metric.key.starts_with("request-context:")).count(),
            count + 1
        );
        let context = observed_text(&finding);
        assert!(context.contains("waiting_marker") && context.contains("output_marker"));
        assert!(!context.contains("9007199254740997"), "a queue position cannot inflate the observed request count");
        assert_eq!(finding.fix.as_ref().unwrap().summary, t!("view.request_memory.output_wait.investigation"));
        assert!(finding.fix.as_ref().unwrap().sql.is_none() && finding.queries.is_empty());
    }
}

#[test]
fn partial_native_reads_never_establish_a_healthy_queue_from_missing_or_unowned_requests() {
    for rows in [
        vec![],
        vec![grant("42", ExecutionGrantState::Granted)],
        vec![unassociated(grant("41", ExecutionGrantState::Waiting))],
    ] {
        let snapshot = partial_snapshot(rows);
        assert!(finding_at_point(&snapshot).is_none());
        assert!(matches!(
            super::super::MemoryQueueCheck.evidence(&snapshot),
            culprit_core::CheckEvidence::Insufficient(_)
        ));
        let report = crate::analyze(snapshot);
        assert!(!report.findings.iter().any(|finding| finding.check == "memory-queue"));
        assert_eq!(
            report.assessments.iter().find(|assessment| assessment.check == "memory-queue").unwrap().status,
            CheckStatus::InsufficientEvidence
        );
    }
}

#[test]
fn partial_native_reads_require_the_original_request_roster_and_generation_proof() {
    for change in [
        "missing-request",
        "insufficient-request",
        "changed-request",
        "missing-statement-identity",
        "missing-session-generation",
        "missing-request-generation",
        "duplicate-request",
    ] {
        let mut row = grant("41", ExecutionGrantState::Waiting);
        match change {
            "missing-request" | "insufficient-request" | "changed-request" => {
                row = unassociated(row);
                row.request_association = match change {
                    "missing-request" => RequestGrantAssociation::MissingRequest,
                    "insufficient-request" => RequestGrantAssociation::InsufficientEvidence,
                    _ => RequestGrantAssociation::ChangedRequest,
                };
            }
            "missing-statement-identity" => row.request.as_mut().unwrap().statement_identity = None,
            "missing-session-generation" => row.request.as_mut().unwrap().key.session.generation.clear(),
            "missing-request-generation" => {
                row.request.as_mut().unwrap().key.request.as_mut().unwrap().generation.clear()
            }
            "duplicate-request" => {}
            _ => unreachable!(),
        }
        let mut rows = vec![row];
        if change == "duplicate-request" {
            let mut duplicate = grant("42", ExecutionGrantState::Granted);
            duplicate.request.as_mut().unwrap().key = rows[0].request.as_ref().unwrap().key.clone();
            rows.push(duplicate);
        }
        assert!(finding_at_point(&partial_snapshot(rows)).is_none(), "{change}");
    }
}

#[test]
fn partial_mars_context_never_borrows_sql_from_a_peer_or_negative_session_generation() {
    let waiter = grant("41", ExecutionGrantState::Waiting);
    let mut peer = grant("42", ExecutionGrantState::Granted);
    peer.request.as_mut().unwrap().key.session = waiter.request.as_ref().unwrap().key.session.clone();
    peer.request.as_mut().unwrap().key.request =
        Some(NativeActivityIdentity { id: "1".into(), generation: "request-2".into() });
    peer.request.as_mut().unwrap().statement =
        DefinitionPayload::sql("SELECT mars_peer_canary FROM dbo.other_rows", Engine::Mssql);
    peer.statement_relationship = GrantStatementRelationship::Unresolved;
    let peer_key = peer.key().unwrap();
    let mut snapshot = partial_snapshot(vec![waiter, peer]);
    let finding = finding_at_point(&snapshot).unwrap();
    let text = observed_text(&finding);
    assert!(text.contains("waiting_marker"));
    assert!(!text.contains("mars_peer_canary"));
    let peer_context =
        finding.metrics.iter().find(|metric| metric.key == format!("request-context:{peer_key}")).unwrap();
    assert_eq!(peer_context.text.as_deref(), Some(t!("check.memory_queue.recorded.statement_unavailable").as_str()));
    captured(&mut snapshot).observations[1].request.as_mut().unwrap().session_context =
        Some(Box::new(ActivitySessionContext {
            lifecycle: SessionLifecycle::PoolResetPrelogin,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
    assert!(captured(&mut snapshot).is_valid(), "negative generation context remains valid retained evidence");
    assert!(finding_at_point(&snapshot).is_none(), "a negative MARS peer withdraws the shared session generation");
}

#[test]
fn the_pipeline_keeps_native_positive_evidence_without_an_unrelated_aggregate_signal() {
    for state in [SourceState::Collected, SourceState::Partial] {
        let mut snapshot = snapshot(vec![grant("41", ExecutionGrantState::Waiting)]);
        snapshot.sources.last_mut().unwrap().state = state;
        assert!(matches!(
            super::super::MemoryQueueCheck.evidence(&snapshot),
            culprit_core::CheckEvidence::Insufficient(_)
        ));
        let report = crate::analyze(snapshot);
        let finding = report.findings.iter().find(|finding| finding.check == "memory-queue").unwrap();
        assert_eq!(finding.actionability, Actionability::Information);
        assert!(observed_text(finding).contains("waiting_marker"));
        assert_eq!(
            report.assessments.iter().find(|assessment| assessment.check == "memory-queue").unwrap().status,
            CheckStatus::Findings
        );
    }
}

#[test]
fn an_independent_aggregate_queue_never_supplies_severity_time_or_sql_authority() {
    let mut snapshot =
        snapshot(vec![grant("41", ExecutionGrantState::Waiting), grant("42", ExecutionGrantState::Granted)]);
    snapshot.pressure.as_mut().unwrap().signals.push(culprit_core::Signal::new(
        culprit_core::signal_id::MEMORY_GRANTS_PENDING,
        "aggregate",
        20.0,
        culprit_core::SignalUnit::Count,
        culprit_core::SignalScope::Now,
    ));
    let native = finding_at_point(&snapshot).unwrap();
    let all = super::super::memory_queue(&snapshot);
    assert_eq!(all.len(), 2, "independent populations cannot be joined by matching counts or a scan container");
    assert_eq!(native.severity, Severity::Low);
    let generic = all.iter().find(|finding| finding.id != native.id).unwrap();
    assert_eq!(generic.severity, Severity::Medium);
    assert!(!observed_text(generic).contains("waiting_marker"));
    let original_id = native.id;
    snapshot.taken_at = "2040-01-01T00:00:00Z".into();
    assert_eq!(
        finding_at_point(&snapshot).unwrap().id,
        original_id,
        "a host clock cannot move the recorded native point"
    );
}

#[test]
fn healthy_and_ordinary_blocking_reservations_do_not_get_output_delivery_advice() {
    assert!(finding_at_point(&snapshot(vec![grant("42", ExecutionGrantState::Granted)])).is_none());
    let mut holder = grant("42", ExecutionGrantState::Granted);
    holder.request.as_mut().unwrap().wait.as_mut().unwrap().native_type = Some("LCK_M_S".into());
    let finding = finding_at_point(&snapshot(vec![grant("41", ExecutionGrantState::Waiting), holder])).unwrap();
    assert_eq!(finding.fix.as_ref().unwrap().summary, t!("view.request_memory.waiting.investigation"));
    assert!(observed_text(&finding).contains("output_marker"), "the observed statement stays participant context");
    assert!(!finding.summary.contains(&t!("check.memory_queue.recorded.output_context")));
}

#[test]
fn missing_foreign_recycled_or_unknown_scope_and_native_clocks_cannot_authorize_context() {
    for state in [SourceState::Collected, SourceState::Partial] {
        for change in [
            "identity",
            "foreign-db",
            "foreign-endpoint",
            "native-engine",
            "scan-database",
            "native-epoch",
            "outside-clock",
            "mixed-clock",
            "missing-clock",
            "source-missing",
            "source-denied",
            "source-empty",
            "source-inapplicable",
            "source-duplicate",
            "recycled-session",
        ] {
            let mut snapshot =
                snapshot(vec![grant("41", ExecutionGrantState::Waiting), grant("42", ExecutionGrantState::Granted)]);
            snapshot.sources.last_mut().unwrap().state = state;
            match change {
                "identity" => snapshot.identity = None,
                "foreign-db" => {
                    let CollectionTarget::Database(database) = &mut captured(&mut snapshot).context.target else {
                        panic!()
                    };
                    database.database = "foreign".into();
                }
                "foreign-endpoint" => {
                    snapshot.identity.as_mut().unwrap().location = culprit_core::DatabaseLocation::Unknown
                }
                "native-engine" => snapshot.server.engine = culprit_core::Engine::Postgres,
                "scan-database" => snapshot.server.database = "foreign".into(),
                "native-epoch" => {
                    let SourcePartition::InstanceLocal { epoch, .. } = &mut captured(&mut snapshot).context.partition
                    else {
                        panic!()
                    };
                    *epoch = None;
                }
                "outside-clock" => captured(&mut snapshot).context.observed_until = "2019-01-01T00:00:00Z".into(),
                "mixed-clock" => captured(&mut snapshot).observations[0].observed_at = "2020-01-01T08:01:01Z".into(),
                "missing-clock" => captured(&mut snapshot).observed_at.clear(),
                "source-missing" => snapshot.sources.retain(|source| source.source != "current request memory"),
                "source-denied" => snapshot.sources.last_mut().unwrap().state = SourceState::Unavailable,
                "source-empty" => snapshot.sources.last_mut().unwrap().state = SourceState::Empty,
                "source-inapplicable" => snapshot.sources.last_mut().unwrap().state = SourceState::NotApplicable,
                "source-duplicate" => snapshot.sources.push(snapshot.sources.last().unwrap().clone()),
                "recycled-session" => {
                    let mut recycled = grant("43", ExecutionGrantState::Waiting);
                    recycled.request.as_mut().unwrap().key.session.id = "41".into();
                    recycled.request.as_mut().unwrap().key.session.generation = "recycled-login".into();
                    captured(&mut snapshot).observations = vec![grant("41", ExecutionGrantState::Waiting), recycled];
                }
                _ => unreachable!(),
            }
            assert!(finding_at_point(&snapshot).is_none(), "{state:?}: {change}");
        }
    }
}

#[test]
fn unrelated_memory_resources_and_unqualified_reservations_keep_the_waiting_request_context() {
    for change in ["foreign-resource", "missing-resource", "zero-grant", "unresolved-request"] {
        let waiter = grant("41", ExecutionGrantState::Waiting);
        let mut holder = grant("42", ExecutionGrantState::Granted);
        match change {
            "foreign-resource" => holder.resource = Some(NativeIdentity::new("foreign-pool")),
            "missing-resource" => holder.resource = None,
            "zero-grant" => holder.granted_kibibytes = Some("0".into()),
            "unresolved-request" => holder = unassociated(holder),
            _ => unreachable!(),
        }
        let finding = finding_at_point(&snapshot(vec![waiter, holder])).unwrap();
        assert_eq!(finding.fix.as_ref().unwrap().summary, t!("view.request_memory.waiting.investigation"), "{change}");
    }
}

#[test]
fn duplicate_full_request_identity_withdraws_the_recorded_case() {
    let waiter = grant("41", ExecutionGrantState::Waiting);
    let mut holder = grant("42", ExecutionGrantState::Granted);
    holder.request.as_mut().unwrap().key = waiter.request.as_ref().unwrap().key.clone();
    let snapshot = snapshot(vec![waiter, holder]);
    assert!(snapshot.pressure.as_ref().unwrap().request_memory.as_ref().unwrap().is_valid());
    assert!(
        finding_at_point(&snapshot).is_none(),
        "a duplicated native request cannot establish separate participants"
    );
}

#[test]
fn temporary_allocations_version_retainers_and_historical_peaks_are_not_reservation_ownership() {
    let allocation_canary = "9007199254740995";
    let peak_canary = u64::MAX.to_string();
    let mut holder = grant("42", ExecutionGrantState::Granted);
    holder.granted_kibibytes = Some("0".into());
    holder.maximum_used_kibibytes = Some(peak_canary.clone());
    let mut snapshot = snapshot(vec![grant("41", ExecutionGrantState::Waiting), holder]);
    snapshot.pressure.as_mut().unwrap().oldest_transaction = Some(OpenTransaction {
        session: "retainer-canary".into(),
        seconds: 3600.0,
        activity: TransactionActivity::Snapshot,
        held_bytes: Some(1073741824.0),
        ..Default::default()
    });
    snapshot.operations.task_space = Some(serde_json::from_value(json!({
        "source":"task-space", "observed_from":"2020-01-01T08:01:00Z","observed_until":"2020-01-01T08:01:00Z", "observations":[{
            "source":"task-space","observed_at":"2020-01-01T08:01:00Z","task":{"session_id":"42","request_id":"0","execution_context_id":"0"},
            "native_database_id":"2","bytes_per_page":8192,"user_allocated_pages":allocation_canary,"user_deallocated_pages":"0","internal_allocated_pages":"0","internal_deallocated_pages":"0"
        }]})).unwrap());
    let finding = finding_at_point(&snapshot).unwrap();
    assert_eq!(finding.fix.as_ref().unwrap().summary, t!("view.request_memory.waiting.investigation"));
    let text = observed_text(&finding);
    assert!(text.contains("9007199254740993"), "the waiter's requested memory remains original native evidence");
    assert!(!text.contains("retainer-canary"));
    assert!(!text.contains("output_marker"));
    assert!(!text.contains(allocation_canary));
    assert!(!text.contains(&peak_canary));
}

#[test]
fn statement_absence_and_later_context_never_borrow_workload_sql() {
    for relationship in [GrantStatementRelationship::Unresolved, GrantStatementRelationship::ObservedCurrentStatement] {
        let mut waiter = grant("41", ExecutionGrantState::Waiting);
        waiter.statement_relationship = relationship;
        waiter.request.as_mut().unwrap().statement =
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
        let finding = finding_at_point(&snapshot(vec![waiter])).unwrap();
        assert!(observed_text(&finding).contains(&t!("check.memory_queue.recorded.statement_unavailable")));
        assert!(!observed_text(&finding).contains("waiting_marker"));
    }
}

#[test]
fn request_count_and_unicode_statement_previews_are_bounded_and_order_independent() {
    let mut rows = Vec::new();
    for id in 41..49 {
        rows.push(grant(&id.to_string(), ExecutionGrantState::Waiting));
    }
    for id in 51..55 {
        rows.push(grant(&id.to_string(), ExecutionGrantState::Granted));
    }
    for row in &mut rows {
        row.request.as_mut().unwrap().statement = DefinitionPayload::sql(
            &format!("SELECT {} FROM dbo.long_statement", "列".repeat(1000)),
            culprit_core::Engine::Mssql,
        );
    }
    let mut snapshot = snapshot(rows);
    let finding = finding_at_point(&snapshot).unwrap();
    assert_eq!(
        finding.metrics.iter().filter(|metric| metric.key.starts_with("request-context:")).count(),
        MAX_DISPLAYED_CONTEXTS
    );
    for metric in finding.metrics.iter().filter(|metric| metric.key.starts_with("request-context:")) {
        assert!(metric.text.as_ref().unwrap().chars().count() <= MAX_STATEMENT_CHARACTERS + 1);
    }
    assert!(finding.caveats.contains(&t!("check.memory_queue.recorded.omitted", count = "7")));
    captured(&mut snapshot).observations.reverse();
    assert_eq!(observed_text(&finding_at_point(&snapshot).unwrap()), observed_text(&finding));
}

#[test]
fn display_redaction_removes_private_literals_without_rewriting_original_native_facts() {
    let mut waiter = grant("41", ExecutionGrantState::Waiting);
    let raw = "SELECT raw_context_marker FROM dbo.orders WHERE private_value=N'private-native-literal-canary'";
    waiter.request.as_mut().unwrap().statement = serde_json::from_value(json!({
        "state":"available", "text":raw, "sha256":digest(raw.as_bytes()), "redaction_version":1
    }))
    .unwrap();
    assert!(waiter.is_valid());
    let snapshot = snapshot(vec![waiter]);
    let original = serde_json::to_value(&snapshot).unwrap();
    let finding = finding_at_point(&snapshot).unwrap();
    let display = observed_text(&finding);
    assert!(display.contains("raw_context_marker"));
    assert!(!display.contains("private-native-literal-canary"));
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), original);
    assert!(original.to_string().contains("private-native-literal-canary"));
}

fn ordinary_reservation(id: &str, granted: Option<&str>) -> ExecutionGrantObservation {
    let mut row = grant(id, ExecutionGrantState::Granted);
    row.granted_kibibytes = granted.map(str::to_owned);
    let request = row.request.as_mut().unwrap();
    request.wait = None;
    request.statement =
        DefinitionPayload::sql(&format!("SELECT reservation_{id} FROM dbo.inputs"), culprit_core::Engine::Mssql);
    assert!(row.is_valid());
    row
}

#[test]
fn concurrent_reservations_identify_original_same_resource_participants_in_both_orders() {
    let rows = vec![
        grant("41", ExecutionGrantState::Waiting),
        ordinary_reservation("42", Some("1024")),
        ordinary_reservation("43", Some("2048")),
    ];
    let mut snapshot = snapshot(rows);
    let original = serde_json::to_value(&snapshot).unwrap();
    let case = decide(&snapshot).unwrap();
    assert!(matches!(case.investigation, QueueInvestigation::Concurrency));
    assert_eq!(case.reservations.len(), 2);
    let finding = render(case);
    assert!(finding.summary.contains(&t!("check.memory_queue.recorded.concurrency")));
    assert!(finding.summary.contains(&t!("check.memory_queue.recorded.reservations", n = 2, count = "2")));
    assert_eq!(finding.fix.as_ref().unwrap().summary, t!("check.memory_queue.recorded.concurrency_investigation"));
    assert_eq!(finding.verify, vec![t!("view.request_memory.verify")]);
    assert_eq!(finding.actionability, Actionability::Information);
    assert_eq!(finding.severity, Severity::Low);
    assert!(finding.queries.is_empty() && finding.fix.as_ref().unwrap().sql.is_none());
    let text = observed_text(&finding);
    assert!(text.contains("waiting_marker") && text.contains("reservation_42") && text.contains("reservation_43"));
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), original);
    captured(&mut snapshot).observations.reverse();
    let reversed = finding_at_point(&snapshot).unwrap();
    assert_eq!(finding.id, reversed.id);
    assert_eq!(finding.summary, reversed.summary);
    assert_eq!(observed_text(&finding), observed_text(&reversed));
    assert_eq!(finding.fix.as_ref().unwrap().summary, reversed.fix.as_ref().unwrap().summary);
}

#[test]
fn concurrency_requires_distinct_granted_requests_on_one_waiting_resource_without_a_size_verdict() {
    for grant_size in ["1024", "1073741824"] {
        let one =
            snapshot(vec![grant("41", ExecutionGrantState::Waiting), ordinary_reservation("42", Some(grant_size))]);
        let case = decide(&one).unwrap();
        assert!(matches!(case.investigation, QueueInvestigation::Waiting));
        let finding = render(case);
        assert!(observed_text(&finding).contains("reservation_42"));
        assert_eq!(finding.fix.as_ref().unwrap().summary, t!("view.request_memory.waiting.investigation"));
    }
    let mut other_waiter = grant("51", ExecutionGrantState::Waiting);
    let mut other_grant = ordinary_reservation("52", Some("2048"));
    let other_resource = Some(NativeIdentity::new("pool-2").qualified("resource_semaphore_id", "1"));
    other_waiter.resource = other_resource.clone();
    other_grant.resource = other_resource;
    let snapshot = snapshot(vec![
        grant("41", ExecutionGrantState::Waiting),
        ordinary_reservation("42", Some("1024")),
        other_waiter,
        other_grant,
    ]);
    let case = decide(&snapshot).unwrap();
    assert_eq!(case.reservations.len(), 2);
    assert!(matches!(case.investigation, QueueInvestigation::Waiting), "different semaphores cannot be pooled");
    let finding = render(case);
    assert!(!finding.summary.contains(&t!("check.memory_queue.recorded.concurrency")));
    assert!(observed_text(&finding).contains("reservation_42") && observed_text(&finding).contains("reservation_52"));
}

#[test]
fn ordinary_reservations_require_positive_current_grants_without_borrowing_waiting_or_peak_values() {
    for missing in [None, Some("0")] {
        let mut unknown = ordinary_reservation("43", missing);
        unknown.maximum_used_kibibytes = Some(u64::MAX.to_string());
        let snapshot = snapshot(vec![
            grant("41", ExecutionGrantState::Waiting),
            grant("44", ExecutionGrantState::Waiting),
            ordinary_reservation("42", Some("1024")),
            unknown,
        ]);
        let case = decide(&snapshot).unwrap();
        assert_eq!(case.reservations.len(), 1);
        assert!(matches!(case.investigation, QueueInvestigation::Waiting));
        let finding = render(case);
        assert!(observed_text(&finding).contains("reservation_42"));
        assert!(!observed_text(&finding).contains("reservation_43"));
        assert!(!observed_text(&finding).contains(&u64::MAX.to_string()));
    }
}

#[test]
fn ordinary_and_output_wait_reservations_share_one_display_budget_without_duplicate_contexts() {
    let mut rows = vec![grant("41", ExecutionGrantState::Waiting), grant("42", ExecutionGrantState::Waiting)];
    rows.push(grant("51", ExecutionGrantState::Granted));
    rows.extend((52..57).map(|id| ordinary_reservation(&id.to_string(), Some("1024"))));
    let mut snapshot = snapshot(rows);
    let finding = finding_at_point(&snapshot).unwrap();
    let contexts =
        finding.metrics.iter().filter(|metric| metric.key.starts_with("request-context:")).collect::<Vec<_>>();
    assert_eq!(contexts.len(), MAX_DISPLAYED_CONTEXTS);
    let keys = contexts.iter().map(|metric| &metric.key).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), contexts.len());
    assert_eq!(contexts.iter().filter(|metric| metric.text.as_ref().unwrap().contains("output_marker")).count(), 1);
    assert!(finding.summary.contains(&t!("check.memory_queue.recorded.concurrency")));
    assert!(finding.summary.contains(&t!("check.memory_queue.recorded.output_context")));
    assert!(
        finding.fix.as_ref().unwrap().summary.contains(&t!("check.memory_queue.recorded.concurrency_investigation"))
    );
    assert!(finding.fix.as_ref().unwrap().summary.contains(&t!("view.request_memory.output_wait.investigation")));
    assert_eq!(finding.verify, vec![t!("view.request_memory.verify"), t!("view.request_memory.output_wait.verify")]);
    assert!(finding.caveats.contains(&t!("check.memory_queue.recorded.omitted", count = "3")));
    captured(&mut snapshot).observations.reverse();
    let reversed = finding_at_point(&snapshot).unwrap();
    assert_eq!(observed_text(&finding), observed_text(&reversed));
}

#[test]
fn ordinary_reservation_context_keeps_existing_clock_scope_generation_and_duplicate_request_fences() {
    for change in ["clock", "unknown-epoch", "foreign-database", "duplicate-request", "different-semaphore"] {
        let mut snapshot = snapshot(vec![
            grant("41", ExecutionGrantState::Waiting),
            ordinary_reservation("42", Some("1024")),
            ordinary_reservation("43", Some("2048")),
        ]);
        let read = captured(&mut snapshot);
        match change {
            "clock" => read.observations[2].observed_at = "2020-01-01T08:01:01Z".into(),
            "unknown-epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut read.context.partition else { unreachable!() };
                *epoch = None;
            }
            "foreign-database" => read.observations[2].request.as_mut().unwrap().database = Some("foreign".into()),
            "duplicate-request" => {
                let key = read.observations[1].request.as_ref().unwrap().key.clone();
                read.observations[2].request.as_mut().unwrap().key = key;
            }
            "different-semaphore" => {
                read.observations[2].resource =
                    Some(NativeIdentity::new("pool-2").qualified("resource_semaphore_id", "1"))
            }
            _ => unreachable!(),
        }
        if change == "different-semaphore" {
            let case = decide(&snapshot).unwrap();
            assert_eq!(case.reservations.len(), 1);
            assert!(matches!(case.investigation, QueueInvestigation::Waiting));
            assert!(!observed_text(&render(case)).contains("reservation_43"));
        } else {
            assert!(finding_at_point(&snapshot).is_none(), "{change}");
        }
    }
}

#[test]
fn partial_concurrent_reservations_preserve_positive_context_with_bounded_redaction_and_no_sql_fallback() {
    let mut first = ordinary_reservation("42", Some("1024"));
    let raw = format!(
        "SELECT N'reservation-secret-canary' AS token,reservation_private_marker,{} FROM dbo.inputs",
        "列".repeat(1000)
    );
    first.request.as_mut().unwrap().statement = serde_json::from_value(json!({
        "state":"available","text":raw,"sha256":digest(raw.as_bytes()),"redaction_version":1
    }))
    .unwrap();
    let mut later_sql = ordinary_reservation("43", Some("2048"));
    later_sql.statement_relationship = GrantStatementRelationship::Unresolved;
    later_sql.request.as_mut().unwrap().statement =
        DefinitionPayload::sql("SELECT later_statement_must_not_be_borrowed", culprit_core::Engine::Mssql);
    let snapshot = partial_snapshot(vec![grant("41", ExecutionGrantState::Waiting), first, later_sql]);
    let original = serde_json::to_value(&snapshot).unwrap();
    let finding = finding_at_point(&snapshot).unwrap();
    assert!(finding.summary.contains("At least 1 request was waiting"));
    assert!(finding.summary.contains(&t!("check.memory_queue.recorded.concurrency")));
    assert!(finding.caveats.contains(&t!("view.memory_participants.limit.source_partial")));
    let text = observed_text(&finding);
    assert!(text.contains("reservation_private_marker"));
    assert!(!text.contains("reservation-secret-canary") && !text.contains("later_statement_must_not_be_borrowed"));
    assert!(text.contains(&t!("check.memory_queue.recorded.statement_unavailable")));
    for metric in finding.metrics.iter().filter(|metric| metric.key.starts_with("request-context:")) {
        assert!(metric.text.as_ref().unwrap().chars().count() <= MAX_STATEMENT_CHARACTERS + 1);
    }
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), original);
}
