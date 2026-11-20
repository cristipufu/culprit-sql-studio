use super::*;
use crate::{
    activity::database_context::tests::row,
    instance::test_support::{epoch, metadata},
    read::{NativeRead, Parameter, ReadError, ReadResult, ReadSession},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use std::sync::Arc;

fn authority(at: u32) -> Context {
    let row = replace(&row(5), &[("observed_at", Val::Text(format!("2020-01-01T06:00:{at:02}Z")))]);
    crate::local_source::test_azure_authority(&row).unwrap()
}
fn self_row(cpu: Option<i64>, memory: Option<i64>) -> Rec {
    Rec::of(&[
        ("collector_session_id", Val::Int(71)),
        ("collector_login_without_offset", Val::Text("2020-01-01T05:30:00.003".into())),
        ("collector_cpu_ms", cpu.map(Val::Int).unwrap_or(Val::Null)),
        ("collector_reads", Val::Int(9_007_199_254_740_993)),
        ("collector_logical_reads", Val::Int(200)),
        ("collector_dirty_marks", Val::Int(3)),
        ("collector_memory_pages", memory.map(Val::Int).unwrap_or(Val::Null)),
    ])
}
fn sample(authority: &Context, row: &Rec, ordinal: u32, generation: u64) -> Observation {
    observation(authority, row, ordinal, Some(generation))
}
fn body(work: &SessionWork) -> MssqlSessionReadWork {
    let native = work.snapshot().unwrap();
    let NativeSessionReadWork::MssqlSession(body) = *native;
    *body
}
fn public(work: &SessionWork) -> NativeSessionReadSummary {
    work.snapshot().unwrap().summary(None).unwrap()
}
fn measured(work: &SessionWork) -> Option<String> {
    let NativeSessionReadSummary::MssqlSession { interval, .. } = public(work);
    interval.and_then(|interval| interval.cpu_ms)
}
fn completed() -> SessionWork {
    let mut work = SessionWork::default();
    work.observe(sample(&authority(0), &self_row(Some(10), Some(30)), 3, 1));
    work.observe(sample(&authority(1), &self_row(Some(17), Some(20)), 9, 1));
    work
}

fn boundary(work: &mut SessionWork, at: u32, cpu: i64, ordinal: u32) {
    let Observation::Point(point) = sample(&authority(at), &self_row(Some(cpu), Some(30)), ordinal, 1) else {
        panic!("native boundary fixture")
    };
    work.source_boundary(&point.authority, point.session, point.point, point.generation);
}

#[test]
fn independent_boundaries_include_the_first_dispatch_and_final_settlement_without_relabeling_self_samples() {
    let mut work = SessionWork::default();
    work.observe(Observation::Started);
    boundary(&mut work, 0, 5, 0);
    work.observe(sample(&authority(1), &self_row(Some(10), Some(30)), 3, 1));
    work.observe(sample(&authority(2), &self_row(Some(17), Some(20)), 9, 1));
    boundary(&mut work, 3, 20, 9);
    assert_eq!(measured(&work), None, "a quiet independent sample cannot settle the original native owner");
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(measured(&work).as_deref(), Some("15"));
    let interval = body(&work).interval.unwrap();
    assert_eq!(interval.initial.query_ordinal, 0);
    assert_eq!(interval.latest.unwrap().query_ordinal, 9);

    let mut ordinary = completed();
    ordinary.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&ordinary).interval.unwrap().initial.query_ordinal, 3);
    assert_eq!(measured(&ordinary).as_deref(), Some("7"));
}

#[test]
fn a_boundary_never_repairs_an_original_scope_session_counter_or_settlement_failure() {
    for failure in ["scope", "session", "counters", "unconfirmed", "missing-context"] {
        let mut work = SessionWork::default();
        work.observe(Observation::Started);
        boundary(&mut work, 0, 5, 0);
        if failure != "missing-context" {
            let source = if failure == "scope" {
                crate::local_source::test_azure_authority(&replace(
                    &row(5),
                    &[
                        ("updateability", Val::Text("READ_ONLY".into())),
                        ("observed_at", Val::Text("2020-01-01T06:00:01Z".into())),
                    ],
                ))
                .unwrap()
            } else {
                authority(1)
            };
            let row = if failure == "session" {
                replace(&self_row(Some(10), Some(30)), &[("collector_session_id", Val::Int(72))])
            } else {
                self_row(Some(10), Some(30))
            };
            work.observe(sample(&source, &row, 3, 1));
        }
        boundary(&mut work, 3, if failure == "counters" { 1 } else { 20 }, 3);
        work.close(if failure == "unconfirmed" { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent }, 1);
        assert_eq!(measured(&work), None, "{failure}");
        assert!(work.snapshot().unwrap().is_valid(), "{failure}");
    }
}

#[test]
fn a_zero_self_sample_or_final_without_a_before_dispatch_boundary_cannot_claim_a_whole_window() {
    let mut work = SessionWork::default();
    work.observe(sample(&authority(0), &self_row(Some(5), Some(30)), 0, 1));
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::InvalidObservation));

    let mut work = completed();
    boundary(&mut work, 3, 20, 9);
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::InvalidObservation));
    assert_eq!(body(&work).interval.unwrap().initial.query_ordinal, 3);
}

#[test]
fn a_partial_self_point_cannot_substitute_for_a_missing_final_independent_boundary() {
    let mut work = SessionWork::default();
    boundary(&mut work, 0, 5, 0);
    work.observe(sample(&authority(1), &self_row(Some(10), Some(30)), 3, 1));
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::MissingFinalObservation));
    assert_eq!(measured(&work), None);
}

#[test]
fn unrelated_source_capability_discovery_does_not_change_independent_boundary_identity_or_relax_self_points() {
    let mut prepared = authority(0);
    prepared.activity.request_page_server_read_capability = PageServerReadAvailability::Unknown;
    let mut captured = authority(1);
    captured.activity.request_page_server_read_capability = PageServerReadAvailability::Available;
    let mut work = SessionWork::default();
    let Observation::Point(point) = sample(&prepared, &self_row(Some(5), Some(30)), 0, 1) else { panic!() };
    work.source_boundary(&point.authority, point.session, point.point, point.generation);
    work.observe(sample(&captured, &self_row(Some(10), Some(30)), 3, 1));
    captured.activity.at = "2020-01-01T06:00:02Z".into();
    work.observe(sample(&captured, &self_row(Some(17), Some(20)), 9, 1));
    prepared.activity.at = "2020-01-01T06:00:03Z".into();
    let Observation::Point(point) = sample(&prepared, &self_row(Some(20), Some(20)), 9, 1) else { panic!() };
    work.source_boundary(&point.authority, point.session, point.point, point.generation);
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(measured(&work).as_deref(), Some("15"));
    assert_eq!(body(&work).interval.unwrap().context.observed_from, "2020-01-01T06:00:00.000000000Z");

    let mut ordinary = SessionWork::default();
    ordinary.observe(sample(&prepared, &self_row(Some(5), Some(30)), 3, 1));
    captured.activity.at = "2020-01-01T06:00:04Z".into();
    ordinary.observe(sample(&captured, &self_row(Some(10), Some(30)), 9, 1));
    ordinary.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&ordinary).issue, Some(NativeReadWorkIssue::SourceChanged));
}

#[test]
fn native_interval_is_provisional_until_the_same_owner_close_is_confirmed() {
    let mut work = completed();
    assert_eq!(measured(&work), None);
    assert_eq!(body(&work).settlement, NativeReadSettlement::Pending);
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(measured(&work).as_deref(), Some("7"));
    let body = body(&work);
    assert!(body.issue.is_none());
    assert_eq!(body.interval.unwrap().latest.unwrap().memory_pages.as_deref(), Some("20"));
}

#[test]
fn socket_and_session_reuse_poison_comparison_even_if_the_native_fields_return() {
    for change in ["socket", "session", "login"] {
        let mut work = SessionWork::default();
        work.observe(sample(&authority(0), &self_row(Some(10), Some(30)), 3, 1));
        let changed = match change {
            "session" => replace(&self_row(Some(11), Some(30)), &[("collector_session_id", Val::Int(72))]),
            "login" => replace(
                &self_row(Some(11), Some(30)),
                &[("collector_login_without_offset", Val::Text("2020-01-01T05:31:00.003".into()))],
            ),
            _ => self_row(Some(11), Some(30)),
        };
        work.observe(sample(&authority(1), &changed, 4, if change == "socket" { 2 } else { 1 }));
        work.observe(sample(&authority(2), &self_row(Some(17), Some(20)), 5, 1));
        work.close(SourceShutdown::Quiescent, 1);
        assert_eq!(measured(&work), None, "{change}");
        assert_eq!(
            body(&work).issue,
            Some(if change == "socket" {
                NativeReadWorkIssue::ConnectionChanged
            } else {
                NativeReadWorkIssue::SessionChanged
            })
        );
    }
}

#[test]
fn role_member_or_process_change_is_retained_even_after_returning_to_original_context() {
    for (field, value) in [
        ("updateability", Val::Text("READ_ONLY".into())),
        ("replica_id", Val::Text("11111111-2222-3333-4444-555555555555".into())),
        ("local_process_id", Val::Int(12)),
    ] {
        let mut work = SessionWork::default();
        work.observe(sample(&authority(0), &self_row(Some(10), Some(30)), 3, 1));
        let changed = crate::local_source::test_azure_authority(&replace(
            &row(5),
            &[(field, value), ("observed_at", Val::Text("2020-01-01T06:00:01Z".into()))],
        ))
        .unwrap();
        work.observe(sample(&changed, &self_row(Some(11), Some(30)), 4, 1));
        work.observe(sample(&authority(2), &self_row(Some(17), Some(20)), 5, 1));
        work.close(SourceShutdown::Quiescent, 1);
        assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::SourceChanged), "{field}");
        assert_eq!(measured(&work), None);
    }
}

#[test]
fn missing_counter_gap_cannot_erase_the_last_known_cumulative_count() {
    for (initial, second, final_cpu) in [(100, None, 90), (100, Some(200), 150)] {
        let mut work = SessionWork::default();
        work.observe(sample(&authority(0), &self_row(Some(initial), Some(30)), 3, 1));
        if let Some(second) = second {
            work.observe(sample(&authority(1), &self_row(Some(second), Some(30)), 4, 1));
        }
        work.observe(sample(&authority(2), &self_row(None, Some(30)), 5, 1));
        work.observe(sample(&authority(3), &self_row(Some(final_cpu), Some(20)), 6, 1));
        work.observe(sample(&authority(4), &self_row(Some(300), Some(20)), 7, 1));
        work.close(SourceShutdown::Quiescent, 1);
        assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::CounterDecreased));
        assert_eq!(measured(&work), None);
        assert!(work.snapshot().unwrap().is_valid());
        let final_text = final_cpu.to_string();
        assert_eq!(
            body(&work).interval.unwrap().latest.unwrap().cpu_ms.as_deref(),
            Some(final_text.as_str()),
            "retain the actual failing observation, not later recovered counters"
        );
    }
}

#[test]
fn disjoint_initial_and_final_counters_are_unavailable_not_a_measured_zero() {
    let initial = replace(
        &self_row(Some(100), None),
        &[("collector_reads", Val::Null), ("collector_logical_reads", Val::Null), ("collector_dirty_marks", Val::Null)],
    );
    let latest =
        replace(&self_row(None, None), &[("collector_logical_reads", Val::Null), ("collector_dirty_marks", Val::Null)]);
    let mut work = SessionWork::default();
    work.observe(sample(&authority(0), &initial, 3, 1));
    work.observe(sample(&authority(1), &latest, 4, 1));
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::MissingCounters));
    assert!(work.snapshot().unwrap().is_valid());
    assert_eq!(measured(&work), None);
}

#[test]
fn unknown_initial_scope_or_later_permission_gap_does_not_borrow_a_later_good_owner() {
    let unknown = crate::local_source::test_azure_authority(&replace(&row(5), &[("replica_id", Val::Null)])).unwrap();
    for unknown_first in [true, false] {
        let mut work = SessionWork::default();
        let initial = authority(0);
        let later = authority(1);
        let first = if unknown_first { &unknown } else { &initial };
        work.observe(sample(first, &self_row(Some(10), Some(30)), 3, 1));
        work.observe(sample(if unknown_first { &later } else { &unknown }, &self_row(Some(11), Some(20)), 4, 1));
        work.observe(sample(&authority(2), &self_row(Some(17), Some(20)), 5, 1));
        work.close(SourceShutdown::Quiescent, 1);
        assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::NativeScopeUnqualified));
        assert_eq!(measured(&work), None);
    }
}

#[test]
fn malformed_optional_self_fields_are_a_metric_gap_and_cannot_poison_native_source_records() {
    for value in
        [Val::Float(2.5), Val::Int(-1), Val::Int(i64::from(i32::MAX) + 1), Val::Text("secret-bad-counter".into())]
    {
        let mut work = SessionWork::default();
        let row = replace(&self_row(Some(10), Some(30)), &[("collector_cpu_ms", value)]);
        work.observe(sample(&authority(0), &row, 3, 1));
        work.close(SourceShutdown::Quiescent, 1);
        assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::InvalidObservation));
        assert!(work.snapshot().unwrap().is_valid());
        assert_eq!(measured(&work), None);
    }
}

#[test]
fn missing_final_observation_and_unconfirmed_close_never_trigger_rescue_or_heal_failure() {
    let mut work = SessionWork::default();
    work.observe(sample(&authority(0), &self_row(Some(10), Some(30)), 3, 1));
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::MissingFinalObservation));
    assert_eq!(measured(&work), None);
    work.close(SourceShutdown::Unconfirmed, 1);
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&work).settlement, NativeReadSettlement::Unconfirmed);
    assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::MissingFinalObservation));
    assert!(work.snapshot().unwrap().has_unconfirmed_work());
}

struct RecordingRead {
    io: FakeRead,
    work: SessionWork,
    generation: u64,
}
#[async_trait::async_trait]
impl NativeRead for RecordingRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.io.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.io.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(self.generation)
    }
    fn observe_session_work(&mut self, observation: Observation) {
        self.work.observe(observation);
    }
    async fn close(&mut self) -> SourceShutdown {
        let shutdown = self.io.close().await;
        self.work.close(shutdown, self.generation);
        shutdown
    }
}
fn context_row(cpu: Option<Val>, at: u32) -> Rec {
    let original = row(3);
    let counters = self_row(Some(10), Some(30));
    let mut base = original.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    base.extend(counters.columns().map(|(key, value)| (key, value.clone())));
    let row = Rec::of(&base);
    let mut updates = vec![("observed_at", Val::Text(format!("2020-01-01T06:00:{at:02}Z")))];
    if let Some(cpu) = cpu {
        updates.push(("collector_cpu_ms", cpu));
    }
    replace(&row, &updates)
}

#[tokio::test]
async fn existing_authority_roundtrips_supply_native_points_without_any_new_query_or_source_owner() {
    let mut io = RecordingRead {
        io: FakeRead::new(vec![
            Ok(vec![metadata()]),
            Ok(vec![epoch(100)]),
            Ok(vec![context_row(None, 0)]),
            Ok(vec![metadata()]),
            Ok(vec![epoch(100)]),
            Ok(vec![context_row(Some(Val::Int(17)), 1)]),
        ]),
        work: SessionWork::default(),
        generation: 1,
    };
    let before = Context::read(&mut io, "fixture.invalid", 1433).await.unwrap();
    let after = before.recheck(&mut io, "fixture.invalid", 1433).await.unwrap();
    assert!(before.same_source(&after));
    assert_eq!(io.queries(), 6);
    assert_eq!(measured(&io.work), None);
    for index in [2, 5] {
        let sql = &io.io.calls[index].sql;
        assert!(sql.contains("collector.session_id=@@SPID"));
        assert!(sql.contains("LEFT JOIN sys.dm_exec_sessions"));
        assert!(sql.contains("OPTION(MAXDOP 1, LOOP JOIN)"));
        assert_eq!(io.io.calls[index].limit, 1);
    }
    assert_eq!(io.close().await, SourceShutdown::Quiescent);
    assert_eq!(io.io.closed, 1);
    assert_eq!(measured(&io.work).as_deref(), Some("7"));
}

#[tokio::test]
async fn missing_or_invalid_optional_counter_fields_leave_an_independent_valid_authority_readable() {
    for cpu in [Val::Null, Val::Float(2.5)] {
        let mut io = RecordingRead {
            io: FakeRead::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![context_row(Some(cpu), 0)])]),
            work: SessionWork::default(),
            generation: 1,
        };
        let before = Context::read(&mut io, "fixture.invalid", 1433).await.unwrap();
        assert!(before.is_qualified());
        assert_eq!(io.queries(), 3);
        assert_eq!(io.close().await, SourceShutdown::Quiescent);
        assert_eq!(measured(&io.work), None);
        assert!(io.work.snapshot().unwrap().is_valid());
    }
}

#[tokio::test]
async fn cancelling_close_preserves_the_native_join_barrier_and_unconfirmed_outcome() {
    let mut config = tiberius::Config::new();
    config.host("127.0.0.1");
    config.port(17383);
    let mut session = ReadSession::new(Arc::new(crate::conn::Pool::new(config, None)), CollectionBudget::default());
    session.connection_generation = 1;
    session.native_work = completed();
    let (entered, mut entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    session.active = Some(tokio::spawn(async move {
        let _ = entered.send(());
        let _ = release_rx.await;
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
    }));
    {
        let closing = session.close();
        tokio::pin!(closing);
        tokio::select! {biased;result=&mut closing=>panic!("close cannot finish before its native owner: {result:?}"),_=&mut entered_rx=>{}}
    }
    assert_eq!(body(&session.native_work).settlement, NativeReadSettlement::Pending);
    assert_eq!(measured(&session.native_work), None);
    assert!(session.active.is_some());
    let _ = release.send(());
    assert_eq!(session.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(body(&session.native_work).settlement, NativeReadSettlement::Unconfirmed);
    assert_eq!(measured(&session.native_work), None);
    assert!(session.read_metrics().unwrap().has_unconfirmed_work());
}

#[tokio::test]
async fn a_stricter_existing_query_cap_never_adds_a_final_counter_rescue_read() {
    let mut io = RecordingRead {
        io: FakeRead::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![context_row(None, 0)])])
            .with_query_limit(3),
        work: SessionWork::default(),
        generation: 1,
    };
    let before = Context::read(&mut io, "fixture.invalid", 1433).await.unwrap();
    let error =
        before.recheck(&mut io, "fixture.invalid", 1433).await.err().expect("final authority query is budget-limited");
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    assert_eq!(io.queries(), 3);
    assert_eq!(io.close().await, SourceShutdown::Quiescent);
    assert_eq!(io.queries(), 3);
    assert_eq!(body(&io.work).issue, Some(NativeReadWorkIssue::NativeScopeUnqualified));
    assert_eq!(measured(&io.work), None);
}

#[tokio::test]
async fn cancelling_a_final_authority_read_cannot_seal_an_earlier_successful_pair() {
    let rows = vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![context_row(None, 0)]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![context_row(Some(Val::Int(17)), 1)]),
    ];
    let mut io =
        RecordingRead { io: FakeRead::new(rows).pause_on_query(7), work: SessionWork::default(), generation: 1 };
    let before = Context::read(&mut io, "fixture.invalid", 1433).await.unwrap();
    before.recheck(&mut io, "fixture.invalid", 1433).await.unwrap();
    assert!(body(&io.work).interval.unwrap().latest.is_some());
    {
        let recheck = before.recheck(&mut io, "fixture.invalid", 1433);
        tokio::pin!(recheck);
        tokio::select! { biased;
            result = &mut recheck => panic!("the source query is deliberately paused: {}", result.is_ok()),
            _ = tokio::task::yield_now() => {}
        }
    }
    assert_eq!(io.queries(), 7);
    assert_eq!(io.close().await, SourceShutdown::Quiescent);
    assert_eq!(io.queries(), 7);
    assert_eq!(body(&io.work).issue, Some(NativeReadWorkIssue::MissingFinalObservation));
    assert_eq!(measured(&io.work), None);
}

#[tokio::test]
async fn rejected_final_authority_rows_retain_the_source_error_and_never_publish_a_native_delta() {
    for case in ["empty", "duplicate", "malformed", "platform"] {
        let final_row = context_row(Some(Val::Int(20)), 2);
        let rows = match case {
            "empty" => vec![],
            "duplicate" => vec![final_row.clone(), final_row],
            "malformed" => vec![replace(&final_row, &[("selected_database_id", Val::Null)])],
            "platform" => vec![replace(&final_row, &[("major", Val::Int(17))])],
            _ => unreachable!(),
        };
        let mut io = RecordingRead {
            io: FakeRead::new(vec![
                Ok(vec![metadata()]),
                Ok(vec![epoch(100)]),
                Ok(vec![context_row(None, 0)]),
                Ok(vec![metadata()]),
                Ok(vec![epoch(100)]),
                Ok(vec![context_row(Some(Val::Int(17)), 1)]),
                Ok(vec![metadata()]),
                Ok(vec![epoch(100)]),
                Ok(rows),
            ]),
            work: SessionWork::default(),
            generation: 1,
        };
        let before = Context::read(&mut io, "fixture.invalid", 1433).await.unwrap();
        before.recheck(&mut io, "fixture.invalid", 1433).await.unwrap();
        let error = before.recheck(&mut io, "fixture.invalid", 1433).await.err().expect(case);
        if case == "platform" {
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        }
        assert_eq!(io.close().await, SourceShutdown::Quiescent);
        assert_eq!(io.queries(), 9);
        assert_eq!(body(&io.work).issue, Some(NativeReadWorkIssue::NativeScopeUnqualified), "{case}");
        assert_eq!(measured(&io.work), None, "{case}");
        assert_eq!(
            body(&io.work).interval.unwrap().latest.unwrap().cpu_ms.as_deref(),
            Some("17"),
            "invalid final metadata never becomes an accepted point"
        );
    }
}

#[test]
fn duplicate_optional_counter_columns_are_unavailable_without_overwriting_original_values() {
    let original = self_row(Some(10), Some(30));
    let mut fields = original.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    fields.push(("collector_cpu_ms", Val::Int(11)));
    let mut work = SessionWork::default();
    work.observe(sample(&authority(0), &Rec::of(&fields), 3, 1));
    work.close(SourceShutdown::Quiescent, 1);
    assert_eq!(body(&work).issue, Some(NativeReadWorkIssue::InvalidObservation));
    assert_eq!(measured(&work), None);
}

#[test]
fn independent_boundaries_cannot_reset_an_already_unconfirmed_original_owner() {
    let mut work = completed();
    work.close(SourceShutdown::Unconfirmed, 1);
    let original = body(&work).interval.unwrap();
    boundary(&mut work, 3, 20, 0);
    boundary(&mut work, 4, 21, 9);
    work.close(SourceShutdown::Quiescent, 1);
    let retained = body(&work);
    assert_eq!(retained.settlement, NativeReadSettlement::Unconfirmed);
    assert_eq!(retained.interval.unwrap(), original);
    assert_eq!(measured(&work), None);
}
