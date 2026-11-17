use super::*;

fn copy() -> CurrentRequestWorkSnapshot {
    let mut input = snapshot();
    input.profile = CurrentRequestWorkProfile::PostgresCopy;
    input.source = "pg_stat_progress_copy".into();
    input.coverage.source = input.source.clone();
    input.context.target = DatabaseIdentity::server(Engine::Postgres, "shop", "fixture.invalid", 5432, None).into();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut input.context.partition else { panic!() };
    member.engine = Engine::Postgres;
    let point = &mut input.observations[0];
    point.actor = ActivityKey {
        session: NativeActivityIdentity { id: "42".into(), generation: "2026-11-15T08:00:00.000001Z".into() },
        request: Some(NativeActivityIdentity {
            id: "statement".into(),
            generation: "2026-11-15T09:00:00.000002Z".into(),
        }),
        transaction: None,
    };
    point.scope = ActivityScopeBasis::SessionDatabase;
    point.statement_identity = culprit_core::engines::postgres_activity_statement_identity(
        &point.actor.session.id,
        &point.actor.session.generation,
        &point.actor.request.as_ref().unwrap().generation,
    );
    point.native = CurrentRequestWorkNative::PostgresCopy {
        progress: Box::new(PostgresCopyProgress {
            database_oid: "16384".into(),
            relation_oid: "0".into(),
            command: PostgresCopyCommand::To,
            io_type: PostgresCopyIo::Pipe,
            bytes_processed: Some("9223372036854775807".into()),
            bytes_total: Some("0".into()),
            tuples_processed: Some("9007199254740993".into()),
            tuples_excluded: Some("0".into()),
            tuples_skipped: None,
        }),
    };
    point.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    input
}

#[test]
fn copy_exact_counts_and_unknown_total_are_not_a_phase_completion_or_temporary_work_diagnosis() {
    let input = copy();
    let report = explain(&input, true).unwrap();
    let point = &report.entries[0];
    for (key, value) in [
        ("view.current_request_work.copy.metric.bytes_processed", "9223372036854775807"),
        ("view.current_request_work.copy.metric.bytes_total", "0"),
        ("view.current_request_work.copy.metric.tuples_processed", "9007199254740993"),
    ] {
        let fact = metric(point, key).unwrap();
        assert_eq!(fact.text.as_deref(), Some(format::digits(value.into()).as_str()));
        assert_eq!(fact.unit, Unit::Text);
    }
    assert!(metric(point, "view.current_request_work.copy.metric.tuples_skipped").is_none());
    assert_eq!(point.explanation, t!("view.current_request_work.copy.entry.query"));
    assert!(report.limitations.contains(&t!("view.current_request_work.copy.limit.total")));
    assert!(!report.limitations.contains(&t!("view.current_request_work.limit.session_database")));
    assert!(!report.limitations.contains(&t!("view.current_request_work.limit.temporary")));
    let json = serde_json::to_value(point).unwrap();
    for field in ["phase", "percent", "completed_at", "spill_bytes", "memory_limit", "cause"] {
        assert!(json.get(field).is_none(), "{field}");
    }
}

#[test]
fn a_filtered_or_skipped_tuple_count_remains_separate_and_zero_does_not_mean_success() {
    let mut input = copy();
    let CurrentRequestWorkNative::PostgresCopy { progress } = &mut input.observations[0].native else { panic!() };
    progress.relation_oid = "16420".into();
    progress.tuples_excluded = Some("3".into());
    progress.tuples_skipped = Some("0".into());
    progress.bytes_processed = None;
    let report = explain(&input, true).unwrap();
    let point = &report.entries[0];
    assert_eq!(point.explanation, t!("view.current_request_work.copy.entry.table"));
    assert!(metric(point, "view.current_request_work.copy.metric.bytes_processed").is_none());
    assert_eq!(
        metric(point, "view.current_request_work.copy.metric.tuples_excluded").unwrap().text.as_deref(),
        Some("3")
    );
    assert_eq!(
        metric(point, "view.current_request_work.copy.metric.tuples_skipped").unwrap().text.as_deref(),
        Some("0")
    );
    assert!(report.limitations.contains(&t!("view.current_request_work.copy.limit.counters")));
}

#[test]
fn copy_partial_read_keeps_native_membership_and_foreign_engine_or_request_is_rejected() {
    let mut input = copy();
    input.observations.clear();
    let report = explain(&input, false).unwrap();
    assert_eq!(report.native_rows, 1);
    assert_eq!(report.observed_rows, 0);
    assert_eq!(report.membership, FocusedRequestMembership::Observed);
    assert_eq!(report.summary, t!("view.current_request_work.summary.read_gap"));
    assert!(explain(&input, true).is_err());
    let mut input = copy();
    input.observations[0].actor.request.as_mut().unwrap().generation = "2026-11-15T09:00:01Z".into();
    assert!(explain(&input, true).is_err());
}

#[test]
fn copy_empty_and_missing_body_keep_their_declared_source_meaning_without_a_missing_event_claim() {
    let mut input = copy();
    input.observations.clear();
    let missing = explain(&input, false).unwrap();
    assert!(missing.limitations.contains(&t!("view.current_request_work.copy.limit.total")));
    input.coverage = StateSourceCoverage::observed(input.source.clone(), 0, false);
    input.result.membership = FocusedRequestMembership::NotObserved;
    let empty = explain(&input, true).unwrap();
    assert_eq!(empty.summary, t!("view.current_request_work.copy.summary.not_observed"));
    assert_ne!(empty.summary, t!("view.current_request_work.summary.not_observed"));
    assert!(empty.limitations.contains(&t!("view.current_request_work.copy.limit.scope")));
    input.profile = CurrentRequestWorkProfile::MysqlStatementEvent;
    assert!(explain(&input, true).is_err(), "an incompatible profile cannot reinterpret an empty COPY read");
}
