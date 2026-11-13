use super::*;
use culprit_core::telemetry::MysqlErrorLogObservation;
use culprit_core::telemetry::{DefinitionPayload, PayloadUnavailable, PostgresLogFailureObservation};

fn row(position: u32, code: Option<&str>) -> NativeFailureRecord {
    NativeFailureRecord::MysqlErrorLog(MysqlErrorLogObservation {
        position,
        logged_at: "2020-01-01T01:02:03.000001Z".into(),
        thread_id: Some("18446744073709551615".into()),
        priority: "Warning".into(),
        error_code: code.map(str::to_owned),
        subsystem: Some("Server".into()),
        message: Some("private text says out of memory and names a query".into()),
    })
}

fn evidence(rows: Vec<NativeFailureRecord>) -> NativeFailureSnapshot {
    NativeFailureSnapshot {
        source: "native-failures".into(),
        observed_from: "2026-11-13T10:00:00Z".into(),
        observed_until: "2026-11-13T10:00:01Z".into(),
        resource: None,
        observations: rows,
    }
}

fn source(evidence: &NativeFailureSnapshot, state: SourceState) -> SourceOutcome {
    SourceOutcome { source: evidence.source.clone(), state, reason: None, reason_message: None }
}

#[test]
fn exact_native_codes_select_the_documented_cases_without_message_priority_or_client_code_inference() {
    let cases = [
        (Some("MY-010935"), NativeFailureMeaning::Allocation, t!("view.native_failures.cause.allocation.title")),
        (Some("MY-010934"), NativeFailureMeaning::Resources, t!("view.native_failures.cause.resources.title")),
        (Some("MY-013131"), NativeFailureMeaning::SortMemory, t!("view.native_failures.cause.sort.title")),
        (Some("1037"), NativeFailureMeaning::Unclassified, t!("view.native_failures.cause.unknown.title")),
        (Some("MY-999999"), NativeFailureMeaning::Unclassified, t!("view.native_failures.cause.unknown.title")),
        (None, NativeFailureMeaning::Unclassified, t!("view.native_failures.cause.unknown.title")),
    ];
    for (code, meaning, title) in cases {
        assert_eq!(NativeFailureMeaning::mysql(code), meaning);
        let evidence = evidence(vec![row(0, code)]);
        let report = explain(&evidence, None).unwrap();
        assert_eq!(report.entries[0].title, title);
        assert!(!report.entries[0].investigation.is_empty());
        assert!(!serde_json::to_string(&report).unwrap().contains("private text"));
    }
}

#[test]
fn retained_diagnostics_preserve_duplicate_positions_order_and_older_utc_events_without_incident_claims() {
    let evidence = evidence(vec![row(8, Some("MY-010935")), row(2, Some("MY-010935"))]);
    let report = explain(&evidence, Some(&source(&evidence, SourceState::Collected))).unwrap();
    assert_eq!(report.entries.iter().map(|entry| entry.position).collect::<Vec<_>>(), [8, 2]);
    assert_eq!(report.summary, t!("view.native_failures.summary.retained", n = 2, count = "2"));
    assert!(report.limitations.contains(&t!("view.native_failures.limit.coverage")));
    assert!(report.limitations.contains(&t!("view.native_failures.limit.attribution")));
    assert!(report.limitations.contains(&t!("view.memory_failures.limit.unowned")));
}

#[test]
fn partial_unavailable_empty_and_unknown_reads_keep_their_actual_qualification() {
    let empty = evidence(vec![]);
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable, SourceState::Partial] {
        let report = explain(&empty, Some(&source(&empty, state))).unwrap();
        assert_eq!(report.source_state, Some(state));
        assert!(report.entries.is_empty());
        assert_eq!(report.summary, t!("view.native_failures.summary.empty"));
    }
    let evidence = evidence(vec![row(0, None)]);
    let mut partial = source(&evidence, SourceState::Partial);
    partial.reason = Some("actual source was bounded".into());
    let report = explain(&evidence, Some(&partial)).unwrap();
    assert_eq!(report.source_reason, partial.reason);
    assert_eq!(report.entries.len(), 1);
    assert!(report.limitations.contains(&t!("view.memory_failures.limit.source_partial")));
    assert!(explain(&evidence, None).unwrap().limitations.contains(&t!("view.memory_failures.limit.source_unknown")));
}

#[test]
fn duplicate_positions_or_contradictory_source_cannot_become_an_explanation() {
    let mut evidence = evidence(vec![row(0, Some("MY-010935"))]);
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        assert!(explain(&evidence, Some(&source(&evidence, state))).is_err());
    }
    let mismatch = SourceOutcome { source: "different-source".into(), ..source(&evidence, SourceState::Collected) };
    assert!(explain(&evidence, Some(&mismatch)).is_err());
    evidence.observations.push(evidence.observations[0].clone());
    assert!(explain(&evidence, None).is_err());
}

fn postgres_row(position: u32, code: &str) -> NativeFailureRecord {
    NativeFailureRecord::PostgresLog(Box::new(PostgresLogFailureObservation {
        position,
        timestamp: "2020-01-01 01:02:03 EET".into(),
        timestamp_utc: None,
        sql_state: code.into(),
        severity: Some("ERROR".into()),
        database: Some("foreign-db".into()),
        process_id: Some("539".into()),
        session_id: Some("5e0be100.21b".into()),
        session_start: Some("2020-01-01 00:00:00 EET".into()),
        session_line_number: Some("7".into()),
        virtual_transaction_id: None,
        transaction_id: None,
        query_id: None,
        leader_process_id: None,
        backend_type: None,
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing },
        deadlock: None,
    }))
}

#[test]
fn postgres_native_conditions_select_specific_explanations_without_cancellation_or_memory_cause_inference() {
    for (code, title) in [
        ("53200", t!("view.native_failures.cause.allocation.title")),
        ("40P01", t!("view.native_failures.postgres.deadlock.title")),
        ("57014", t!("view.native_failures.postgres.canceled.title")),
        ("53000", t!("view.native_failures.postgres.resources.title")),
        ("53100", t!("view.native_failures.postgres.disk.title")),
        ("53300", t!("view.native_failures.postgres.connections.title")),
        ("53400", t!("view.native_failures.postgres.configuration.title")),
        ("54001", t!("view.native_failures.postgres.program.title")),
        ("55P03", t!("view.native_failures.postgres.lock.title")),
        ("57P01", t!("view.native_failures.postgres.admin.title")),
        ("57P02", t!("view.native_failures.postgres.crash.title")),
        ("57P03", t!("view.native_failures.postgres.unavailable.title")),
        ("57P04", t!("view.native_failures.postgres.dropped.title")),
        ("57P05", t!("view.native_failures.postgres.idle_session.title")),
        ("25P03", t!("view.native_failures.postgres.idle_transaction.title")),
        ("25P04", t!("view.native_failures.postgres.transaction_timeout.title")),
        ("P0001", t!("view.native_failures.cause.unknown.title")),
    ] {
        let evidence = evidence(vec![postgres_row(3, code)]);
        let report = explain(&evidence, Some(&source(&evidence, SourceState::Collected))).unwrap();
        assert_eq!(report.entries[0].title, title, "{code}");
        assert!(!report.entries[0].investigation.is_empty());
        assert!(report.limitations.contains(&t!("view.native_failures.postgres.limit.identity")));
        assert!(report.limitations.contains(&t!("view.native_failures.postgres.limit.clock")));
        assert!(report.limitations.contains(&t!("view.memory_failures.limit.unowned")));
    }
    let cancellation = evidence(vec![postgres_row(0, "57014")]);
    assert_eq!(
        explain(&cancellation, None).unwrap().entries[0].explanation,
        t!("view.native_failures.postgres.canceled.explanation")
    );
}

#[test]
fn postgres_duplicate_rows_foreign_database_context_and_unknown_clocks_remain_retained_observations() {
    let evidence = evidence(vec![postgres_row(2, "40P01"), postgres_row(8, "40P01")]);
    let report = explain(&evidence, Some(&source(&evidence, SourceState::Partial))).unwrap();
    assert_eq!(report.entries.iter().map(|entry| entry.position).collect::<Vec<_>>(), [2, 8]);
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert!(evidence.resource.is_none());
    assert!(
        report
            .entries
            .iter()
            .all(|entry| entry.explanation == t!("view.native_failures.postgres.deadlock.explanation"))
    );
    assert!(!serde_json::to_string(&report).unwrap().contains("foreign-db"), "native rows own affected context once");
}
