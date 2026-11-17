use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode, SourceState, Unit};

fn snapshot() -> CurrentRequestWorkSnapshot {
    let actor = ActivityKey {
        session: NativeActivityIdentity { id: "42".into(), generation: "107".into() },
        request: Some(NativeActivityIdentity { id: "3".into(), generation: "0".into() }),
        transaction: None,
    };
    let statement_identity =
        culprit_core::engines::mysql_activity_statement_identity(&actor.session, actor.request.as_ref().unwrap())
            .unwrap();
    CurrentRequestWorkSnapshot {
        profile: CurrentRequestWorkProfile::MysqlStatementEvent,
        source: "mysql-current-request-work".into(),
        context: NativeReadContext {
            target: DatabaseIdentity::server(Engine::Mysql, "shop", "fixture.invalid", 3306, None).into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Mysql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-instance-namespace"),
                    native: NativeIdentity::new("native-instance"),
                }),
                epoch: Some("native-process".into()),
            },
            observed_from: "2026-11-15T10:00:00Z".into(),
            observed_until: "2026-11-15T10:00:01Z".into(),
            database_writable: Some(true),
        },
        coverage: StateSourceCoverage::collected("mysql-current-request-work".into(), 1),
        result: FocusedRequestResult {
            focus: None,
            database_writable: Some(true),
            binding: FocusedRequestBinding::Independent,
            membership: FocusedRequestMembership::Observed,
        },
        observations: vec![CurrentRequestWorkObservation {
            observed_at: "2026-11-15T10:00:00.5Z".into(),
            actor,
            database: "shop".into(),
            scope: ActivityScopeBasis::RequestDatabase,
            statement_identity,
            statement: DefinitionPayload::sql("SELECT id FROM orders WHERE id=9264", Engine::Mysql),
            native: CurrentRequestWorkNative::Mysql {
                work: Box::new(MysqlCurrentRequestWork {
                    thread_id: "107".into(),
                    statement_event_id: "3".into(),
                    timer_start_ps: "0".into(),
                    timer_wait_ps: Some("18446744073709551615".into()),
                    rows_examined: Some("9007199254740993".into()),
                    rows_sent: Some("0".into()),
                    created_tmp_tables: Some("1".into()),
                    created_tmp_disk_tables: Some("0".into()),
                    sort_merge_passes: None,
                    stage: MysqlStageObservation::Observed {
                        event: Box::new(MysqlStageEvent {
                            thread_id: "107".into(),
                            event_id: "4".into(),
                            nesting_event_id: "3".into(),
                            nesting_event_type: "STATEMENT".into(),
                            event_name: "stage/sql/copy to tmp table".into(),
                            timer_start_ps: None,
                            timer_wait_ps: None,
                            work_completed: Some("9007199254740993".into()),
                            work_estimated: Some("42".into()),
                        }),
                    },
                }),
            },
        }],
    }
}

fn metric<'a>(entry: &'a CurrentRequestWorkEntry, key: &str) -> Option<&'a Metric> {
    entry.metrics.iter().find(|metric| metric.is(key))
}

#[test]
fn native_event_counters_and_stage_estimates_remain_exact_without_a_percent_or_memory_diagnosis() {
    let input = snapshot();
    let report = explain(&input, true).unwrap();
    let entry = &report.entries[0];
    for (key, value) in [
        ("view.current_request_work.metric.timer_wait_ps", "18446744073709551615"),
        ("view.current_request_work.metric.rows_examined", "9007199254740993"),
        ("view.current_request_work.metric.stage_work_completed", "9007199254740993"),
        ("view.current_request_work.metric.stage_work_estimated", "42"),
        ("view.current_request_work.metric.timer_start_ps", "0"),
    ] {
        let metric = metric(entry, key).unwrap();
        assert_eq!(metric.text.as_deref(), Some(format::digits(value.into()).as_str()));
        assert_eq!(metric.unit, Unit::Text);
    }
    assert_eq!(entry.actor, input.observations[0].actor);
    assert_eq!(entry.statement_identity, input.observations[0].statement_identity);
    assert_eq!(entry.statement, input.observations[0].statement);
    assert!(report.limitations.contains(&t!("view.current_request_work.limit.stage")));
    assert!(entry.explanation.contains(&t!("view.current_request_work.stage.untimed")));
    let json = serde_json::to_value(&report).unwrap();
    for field in ["native", "percent", "spill_bytes", "spill_ms", "memory_limit", "cause", "completed_at"] {
        assert!(json.get(field).is_none() && json["entries"][0].get(field).is_none(), "{field}");
    }
}

#[test]
fn temporary_and_sort_mechanisms_are_distinct_and_all_zero_or_missing_is_not_a_health_verdict() {
    for (temporary, sort, message) in [
        (Some("1"), None, t!("view.current_request_work.entry.temporary")),
        (None, Some("2"), t!("view.current_request_work.entry.sort")),
        (Some("1"), Some("2"), t!("view.current_request_work.entry.mixed")),
        (Some("0"), Some("0"), t!("view.current_request_work.entry.observed")),
        (None, None, t!("view.current_request_work.entry.observed")),
    ] {
        let mut input = snapshot();
        let native = input.observations[0].native.mysql_mut().unwrap();
        native.created_tmp_tables = temporary.map(str::to_owned);
        native.created_tmp_disk_tables = None;
        native.sort_merge_passes = sort.map(str::to_owned);
        let report = explain(&input, true).unwrap();
        assert!(report.entries[0].explanation.starts_with(&message));
        assert_eq!(
            metric(&report.entries[0], "view.current_request_work.metric.created_tmp_tables").is_some(),
            temporary.is_some()
        );
        assert_eq!(
            metric(&report.entries[0], "view.current_request_work.metric.sort_merge_passes").is_some(),
            sort.is_some()
        );
        assert!(metric(&report.entries[0], "view.current_request_work.metric.created_tmp_disk_tables").is_none());
    }
}

#[test]
fn stage_parent_unknown_disabled_denied_and_not_observed_are_separate_without_borrowed_work() {
    for (stage, expected) in [
        (MysqlStageObservation::NotObserved, t!("view.current_request_work.stage.not_observed")),
        (MysqlStageObservation::Unlinked, t!("view.current_request_work.stage.unlinked")),
        (
            MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::InstrumentationDisabled },
            t!("view.current_request_work.stage.disabled"),
        ),
        (
            MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::PermissionDenied },
            t!("view.current_request_work.stage.denied"),
        ),
        (
            MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::Unsupported },
            t!("view.current_request_work.stage.unsupported"),
        ),
        (
            MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::NotCollected },
            t!("view.current_request_work.stage.not_collected"),
        ),
    ] {
        let mut input = snapshot();
        input.observations[0].native.mysql_mut().unwrap().stage = stage;
        let report = explain(&input, true).unwrap();
        let entry = &report.entries[0];
        assert!(entry.explanation.ends_with(&expected));
        assert!(entry.metrics.iter().all(|metric| !metric.key.contains(".stage_")));
        assert!(metric(entry, "view.current_request_work.metric.created_tmp_tables").is_some());
    }
    let mut input = snapshot();
    let MysqlStageObservation::Observed { event } = &mut input.observations[0].native.mysql_mut().unwrap().stage else {
        panic!()
    };
    event.nesting_event_id = "2".into();
    assert!(explain(&input, true).is_err(), "a mismatched native parent cannot supply stage work");
}

#[test]
fn missing_rows_preserve_original_native_membership_and_counts_without_a_fake_zero_or_complete_empty() {
    let mut input = snapshot();
    input.observations.clear();
    let report = explain(&input, false).unwrap();
    assert_eq!(report.native_rows, 1);
    assert_eq!(report.observed_rows, 0);
    assert_eq!(report.membership, FocusedRequestMembership::Observed);
    assert_eq!(report.summary, t!("view.current_request_work.summary.read_gap"));
    assert!(report.entries.is_empty());
    assert!(report.limitations.contains(&t!("view.current_request_work.limit.read")));
    assert!(explain(&input, true).is_err());
    input.coverage = StateSourceCoverage::observed(input.source.clone(), 0, false);
    input.result.membership = FocusedRequestMembership::NotObserved;
    let absent = explain(&input, true).unwrap();
    assert_eq!(absent.summary, t!("view.current_request_work.summary.not_observed"));
    input.coverage = StateSourceCoverage::unavailable(input.source.clone(), ErrorCode::PermissionDenied, None);
    input.result.membership = FocusedRequestMembership::Unavailable;
    let denied = explain(&input, true).unwrap();
    assert_eq!(denied.source_state, SourceState::Unavailable);
    assert_eq!(denied.summary, t!("view.current_request_work.summary.unavailable"));
    assert_ne!(denied.summary, absent.summary);
}

#[test]
fn schema_context_zero_examined_and_unavailable_sql_keep_their_limits() {
    let mut input = snapshot();
    let row = &mut input.observations[0];
    row.scope = ActivityScopeBasis::SessionDatabase;
    row.native.mysql_mut().unwrap().rows_examined = Some("0".into());
    row.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    let report = explain(&input, true).unwrap();
    for message in [
        t!("view.current_request_work.limit.session_database"),
        t!("view.current_request_work.limit.rows_examined"),
        t!("view.current_request_work.limit.statement"),
    ] {
        assert!(report.limitations.contains(&message));
    }
    assert!(matches!(
        report.entries[0].statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
    ));
    assert_eq!(
        metric(&report.entries[0], "view.current_request_work.metric.rows_examined").unwrap().text.as_deref(),
        Some("0")
    );
}

#[test]
fn contradictory_native_schema_event_or_header_is_rejected_before_explanation() {
    for changed in ["database", "event", "clock", "source", "count"] {
        let mut input = snapshot();
        match changed {
            "database" => input.observations[0].database = "another_database".into(),
            "event" => input.observations[0].native.mysql_mut().unwrap().statement_event_id = "5".into(),
            "clock" => input.observations[0].observed_at = "2026-11-15T10:00:02Z".into(),
            "source" => input.coverage.source = "another-source".into(),
            _ => input.coverage.records = 0,
        }
        assert!(explain(&input, false).is_err(), "{changed}");
    }
}

#[test]
fn unknown_native_member_keeps_positive_partial_work_without_cross_read_authority() {
    let mut input = snapshot();
    input.context.partition = SourcePartition::InstanceLocal { member: None, epoch: Some("native-process".into()) };
    input.coverage = StateSourceCoverage::observed(input.source.clone(), 1, false);
    let report = explain(&input, true).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.source_state, SourceState::Partial);
    assert!(report.limitations.contains(&t!("view.current_request_work.limit.current_scope")));
}

#[test]
fn zero_stage_timer_and_unavailable_work_units_do_not_become_an_untimed_or_complete_stage() {
    let mut input = snapshot();
    input.observations[0].native.mysql_mut().unwrap().timer_wait_ps = None;
    let MysqlStageObservation::Observed { event } = &mut input.observations[0].native.mysql_mut().unwrap().stage else {
        panic!()
    };
    event.timer_wait_ps = Some("0".into());
    event.work_completed = None;
    event.work_estimated = None;
    let report = explain(&input, true).unwrap();
    let entry = &report.entries[0];
    assert!(metric(entry, "view.current_request_work.metric.timer_wait_ps").is_none());
    assert_eq!(
        metric(entry, "view.current_request_work.metric.stage_timer_wait_ps").unwrap().text.as_deref(),
        Some("0")
    );
    assert!(metric(entry, "view.current_request_work.metric.stage_work_completed").is_none());
    assert!(metric(entry, "view.current_request_work.metric.stage_work_estimated").is_none());
    assert!(!entry.explanation.contains(&t!("view.current_request_work.stage.untimed")));
    assert!(report.limitations.contains(&t!("view.current_request_work.limit.stage")));
}

mod postgres;
