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
fn the_pipeline_keeps_native_positive_evidence_without_an_unrelated_aggregate_signal() {
    let snapshot = snapshot(vec![grant("41", ExecutionGrantState::Waiting)]);
    assert!(matches!(super::super::MemoryQueueCheck.evidence(&snapshot), culprit_core::CheckEvidence::Insufficient(_)));
    let report = crate::analyze(snapshot);
    let finding = report.findings.iter().find(|finding| finding.check == "memory-queue").unwrap();
    assert_eq!(finding.actionability, Actionability::Information);
    assert!(observed_text(finding).contains("waiting_marker"));
    assert_eq!(
        report.assessments.iter().find(|assessment| assessment.check == "memory-queue").unwrap().status,
        CheckStatus::Findings
    );
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
    assert!(!observed_text(&finding).contains("output_marker"));
}

#[test]
fn missing_foreign_recycled_or_unknown_scope_and_native_clocks_cannot_authorize_context() {
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
        "source-partial",
        "source-denied",
        "source-duplicate",
        "recycled-session",
    ] {
        let mut snapshot =
            snapshot(vec![grant("41", ExecutionGrantState::Waiting), grant("42", ExecutionGrantState::Granted)]);
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
            "source-partial" => snapshot.sources.last_mut().unwrap().state = SourceState::Partial,
            "source-denied" => snapshot.sources.last_mut().unwrap().state = SourceState::Unavailable,
            "source-duplicate" => snapshot.sources.push(snapshot.sources.last().unwrap().clone()),
            "recycled-session" => {
                let mut recycled = grant("43", ExecutionGrantState::Waiting);
                recycled.request.as_mut().unwrap().key.session.id = "41".into();
                recycled.request.as_mut().unwrap().key.session.generation = "recycled-login".into();
                captured(&mut snapshot).observations = vec![grant("41", ExecutionGrantState::Waiting), recycled];
            }
            _ => unreachable!(),
        }
        assert!(finding_at_point(&snapshot).is_none(), "{change}");
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
            "unresolved-request" => {
                holder.request = None;
                holder.scope = None;
                holder.request_association = RequestGrantAssociation::MissingRequest;
                holder.statement_relationship = GrantStatementRelationship::Unresolved;
            }
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
        MAX_AFFECTED_REQUESTS + MAX_OUTPUT_RESERVATIONS
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
