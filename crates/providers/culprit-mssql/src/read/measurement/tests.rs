use super::*;
use crate::value::Val;

#[test]
fn only_an_active_sample_of_the_exact_template_establishes_cancellation_observation() {
    let sql = "SELECT /* culprit */ database_id FROM sys.databases";
    let watch = TemplateObservation { id: digest(sql.as_bytes()), observed: Arc::new(AtomicBool::new(false)) };
    watch.observe(sql, false);
    watch.observe("SELECT /* culprit */ name FROM sys.databases", true);
    assert!(!watch.was_active());
    watch.observe(sql, true);
    assert!(watch.was_active());
}

#[test]
fn native_deltas_keep_integer_precision_and_refuse_regressing_lifetimes() {
    let before = Counters { cpu_ms: 2, logical_reads: 9_007_199_254_740_993, reads: 3, dirty_marks: 4 };
    let after = Counters { cpu_ms: 2, logical_reads: 9_007_199_254_740_994, reads: 4, dirty_marks: 4 };
    let delta = before.delta(&after).unwrap();
    assert_eq!(delta["logical_reads"], "1");
    assert_eq!(delta["reads"], "1");
    assert_eq!(delta["cpu_ms"], "0");
    assert!(delta["cpu_resolution"].as_str().unwrap().contains("not a zero-cost"));
    assert!(after.delta(&before).is_err());
}

#[test]
fn changed_transport_ownership_cannot_authorize_even_plausible_native_counter_deltas() {
    let before = Counters { cpu_ms: 1, logical_reads: 10, reads: 1, dirty_marks: 0 };
    let after = Counters { cpu_ms: 2, logical_reads: 12, reads: 1, dirty_marks: 0 };
    assert_eq!(qualified_delta(&before, &after, 1, 1).unwrap()["logical_reads"], "2");
    let changed = qualified_delta(&before, &after, 1, 2).unwrap_err();
    assert_eq!(changed.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)));
}

#[test]
fn missing_negative_or_inexact_native_counters_cannot_become_zero_cost_samples() {
    for value in [Val::Null, Val::Int(-1), Val::Float(1.0)] {
        let row = Rec::of(&[
            ("cpu_ms", value),
            ("logical_reads", Val::Int(0)),
            ("reads", Val::Int(0)),
            ("dirty_marks", Val::Int(0)),
        ]);
        assert!(Counters::read(&row).is_err());
    }
    let zero = Rec::of(&[
        ("cpu_ms", Val::Int(0)),
        ("logical_reads", Val::Int(0)),
        ("reads", Val::Int(0)),
        ("dirty_marks", Val::Int(0)),
    ]);
    assert_eq!(Counters::read(&zero).unwrap().cpu_ms, 0);
}

#[test]
fn an_unobserved_grant_is_unknown_and_a_wait_sentinel_does_not_identify_a_blocker() {
    let mut phase = Phase {
        sql: "SELECT 1".into(),
        parameters: json!([]),
        row_limit: 1,
        before: Counters { cpu_ms: 0, logical_reads: 0, reads: 0, dirty_marks: 0 },
        started: Instant::now(),
        active_samples: 0,
        waits: Default::default(),
        blocking_samples: 0,
        maximum_sampled_granted_kb: None,
        maximum_sampled_requested_kb: None,
        maximum_sampled_used_kb: None,
        observer_error: None,
    };
    phase.observe(&Sample {
        observed_at: "2020-01-01T06:00:00Z".into(),
        counters: phase.before.clone(),
        memory_pages: None,
        active: true,
        wait: Some("PAGELATCH_EX".into()),
        blocker: Some(-5),
        granted_kb: None,
        requested_kb: None,
        used_kb: None,
    });
    assert_eq!(phase.blocking_samples, 0);
    assert_eq!(phase.maximum_sampled_granted_kb, None);
    phase.observe(&Sample {
        observed_at: "2020-01-01T06:00:00Z".into(),
        counters: phase.before.clone(),
        memory_pages: None,
        active: true,
        wait: Some("RESOURCE_SEMAPHORE".into()),
        blocker: None,
        granted_kb: Some(0),
        requested_kb: Some(1024),
        used_kb: Some(0),
    });
    assert_eq!(phase.maximum_sampled_granted_kb, Some(0));
    assert_eq!(phase.maximum_sampled_requested_kb, Some(1024));
}

fn preparation_reader() -> crate::resources::test_support::FakeRead {
    use crate::{
        activity::database_context::tests::row,
        instance::test_support::{epoch, metadata},
    };
    crate::resources::test_support::FakeRead::new(vec![
        Ok(vec![Rec::of(&[("session_id", Val::Int(71)), ("login_time", Val::Text("2020-01-01T05:30:00.003".into()))])]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![row(3)]),
    ])
}

#[tokio::test]
async fn preparation_uses_the_explicit_source_endpoint_and_preserves_the_callers_owner() {
    let mut owner = preparation_reader();
    let prepared = prepare(&mut owner, "routed-source.invalid", 3344).await.unwrap();
    let database = prepared.context.target.database().unwrap();
    assert_eq!(
        database.location,
        culprit_core::DatabaseLocation::Server { host: "routed-source.invalid".into(), port: 3344, instance: None }
    );
    assert!(prepared.context.has_qualified_local_owner());
    assert_eq!(prepared.identity.id, 71);
    assert_eq!(prepared.report["queries"], 4);
    assert_eq!(owner.queries(), 4);
    assert_eq!(owner.closed, 0, "a completed preparation retains the caller's original native owner");
    assert!(owner.calls.iter().all(|call| !call.sql.contains("routed-source.invalid")));
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    assert_eq!(owner.closed, 1);
}

#[tokio::test]
async fn an_independent_observer_qualifies_its_native_owner_without_charging_or_replacing_source_work() {
    use crate::{
        activity::database_context::tests::row,
        instance::test_support::{epoch, metadata},
    };
    let mut owner = preparation_reader();
    let prepared = prepare(&mut owner, "routed-source.invalid", 3344).await.unwrap();
    let mut observer = crate::resources::test_support::FakeRead::new(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![row(3)]),
    ]);
    let scope = observer_scope(&mut observer, &prepared.context, "routed-source.invalid", 3344).await.unwrap();
    assert!(scope.matches_earlier_scope(&prepared.context));
    assert_eq!(observer.queries(), 3);
    assert_eq!(owner.queries(), 4, "all existing source preparation remains separately charged");
    assert_eq!(observer.closed, 0);
    assert_eq!(owner.closed, 0);
}

#[tokio::test]
async fn equal_endpoint_labels_cannot_repair_foreign_unknown_or_reversed_observer_scope() {
    use crate::{
        activity::database_context::tests::row,
        instance::test_support::{epoch, metadata},
        resources::test_support::replace,
    };
    let mut owner = preparation_reader();
    let original = prepare(&mut owner, "routed-source.invalid", 3344).await.unwrap().context;
    for change in ["member", "process", "database", "role", "unknown-owner", "unknown-role", "clock"] {
        let mut source = original.clone();
        let mut member = metadata();
        let mut database = row(3);
        match change {
            "member" => member = replace(&member, &[("server_name", Val::Text("foreign-instance".into()))]),
            "process" => member = replace(&member, &[("process_id", Val::Int(43))]),
            "database" => {
                database =
                    replace(&database, &[("database_guid", Val::Text("11111111-2222-3333-4444-555555555555".into()))])
            }
            "role" => database = replace(&database, &[("updateability", Val::Text("READ_ONLY".into()))]),
            "unknown-owner" => source.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "unknown-role" => source.database_writable = None,
            "clock" => source.observed_until = "2020-01-01T06:00:01Z".into(),
            _ => unreachable!(),
        }
        let mut observer = crate::resources::test_support::FakeRead::new(vec![
            Ok(vec![member]),
            Ok(vec![epoch(100)]),
            Ok(vec![database]),
        ]);
        assert!(observer_scope(&mut observer, &source, "routed-source.invalid", 3344).await.is_err(), "{change}");
        assert_eq!(observer.queries(), 3, "{change}: no discovery retry or native rescue");
        assert_eq!(owner.queries(), 4);
    }
}

#[tokio::test]
async fn invalid_source_endpoints_send_no_preparation_sql_and_settle_the_original_owner() {
    for (host, port) in [("", 1433), (" \t", 1433), ("invalid\0source", 1433), ("fixture.invalid", 0)] {
        let mut owner = crate::resources::test_support::FakeRead::new(vec![]);
        let error = prepare_owned(&mut owner, host, port).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
        assert_eq!(owner.queries(), 0);
        assert_eq!(owner.closed, 1);
    }
}

#[tokio::test]
async fn preparation_failure_awaits_the_same_owner_without_replacement_or_error_suppression() {
    for shutdown in [SourceShutdown::Quiescent, SourceShutdown::Unconfirmed] {
        let mut owner =
            crate::resources::test_support::FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
        owner.shutdown = shutdown;
        let error = prepare_owned(&mut owner, "fixture.invalid", 1433).await.err().unwrap();
        assert_eq!(
            CollectionStop::from_error(&error),
            if shutdown == SourceShutdown::Unconfirmed {
                CollectionStop::CancellationUnconfirmed
            } else {
                CollectionStop::TimedOut
            }
        );
        assert_eq!(owner.queries(), 1, "no metadata retry, replacement reader or observer source SQL");
        assert_eq!(owner.closed, 1);
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_preparation_borrow_leaves_the_original_owner_for_explicit_settlement() {
    let mut owner = preparation_reader().pause_on_query(1);
    owner.shutdown = SourceShutdown::Unconfirmed;
    {
        let preparation = prepare_owned(&mut owner, "fixture.invalid", 1433);
        tokio::pin!(preparation);
        tokio::select! {
            biased;
            result = &mut preparation => panic!("the original preparation remains pending: {}", result.is_ok()),
            _ = tokio::time::sleep(Duration::from_millis(1)) => {},
        }
    }
    assert_eq!(owner.queries(), 1);
    assert_eq!(owner.closed, 0, "cancelling a borrowed future cannot replace or pretend to close its owner");
    assert_eq!(owner.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(owner.queries(), 1);
    assert_eq!(owner.closed, 1);
}

struct ObserverRoute(std::sync::Mutex<Vec<(String, u16)>>);
#[async_trait::async_trait]
impl culprit_core::ConnectionTransport for ObserverRoute {
    async fn endpoint(&self, host: &str, port: u16) -> culprit_core::Result<culprit_core::ConnectionEndpoint> {
        self.0.lock().unwrap().push((host.into(), port));
        Err(culprit_core::Error::new(ErrorCode::ConnectionFailed, "fixture route stops before network I/O"))
    }
}

#[tokio::test]
async fn held_observer_acquisition_uses_its_supplied_pool_route_and_cannot_fence_the_source_pool() {
    let observer_route = Arc::new(ObserverRoute(std::sync::Mutex::new(vec![])));
    let mut observer_config = tiberius::Config::new();
    observer_config.host("observer-route.invalid");
    observer_config.port(3345);
    let observer_pool = Arc::new(Pool::new(observer_config, Some(observer_route.clone())));
    let source_pool = Pool::new(tiberius::Config::new(), None);
    let error = HeldObserver::acquire(observer_pool.clone()).await.err().unwrap();
    assert_eq!(error.code, ErrorCode::ConnectionFailed);
    assert_eq!(*observer_route.0.lock().unwrap(), vec![("observer-route.invalid".into(), 3345)]);
    assert_eq!(observer_pool.source_shutdown(), SourceShutdown::Quiescent, "the route refused before native login");
    assert_eq!(source_pool.source_shutdown(), SourceShutdown::Quiescent);
    observer_pool.mark_uncertain();
    let error = HeldObserver::acquire(observer_pool.clone()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(observer_route.0.lock().unwrap().len(), 1, "a fenced observer pool cannot initiate another route");
    assert_eq!(source_pool.source_shutdown(), SourceShutdown::Quiescent);
}

#[test]
fn native_read_counts_and_dirty_marks_keep_the_core_session_meaning_without_storage_bytes() {
    let before = Counters { cpu_ms: 1, logical_reads: 9_007_199_254_740_993, reads: 10, dirty_marks: 20 };
    let after = Counters { cpu_ms: 2, logical_reads: 9_007_199_254_740_994, reads: 12, dirty_marks: 23 };
    let delta = before.delta(&after).unwrap();
    assert_eq!(delta["reads"], "2");
    assert_eq!(delta["dirty_marks"], "3");
    assert!(delta.get("physical_reads").is_none());
    assert!(delta.get("writes").is_none());
    assert!(delta.get("bytes").is_none());
    assert_eq!(before.reads, 10);
    assert_eq!(after.dirty_marks, 23);
}

struct SettlingPreparation {
    native: crate::resources::test_support::FakeRead,
    closing: Option<tokio::sync::oneshot::Sender<()>>,
    settled: tokio::sync::oneshot::Receiver<()>,
}
#[async_trait::async_trait]
impl NativeRead for SettlingPreparation {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        self.native.query(sql, parameters, row_limit).await
    }
    fn queries(&self) -> u32 {
        self.native.queries()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closing.take().unwrap().send(()).unwrap();
        (&mut self.settled).await.unwrap();
        self.native.close().await
    }
}

#[tokio::test]
async fn preparation_failure_cannot_return_before_original_close_finishes() {
    let (closing, close_started) = tokio::sync::oneshot::channel();
    let (release, settled) = tokio::sync::oneshot::channel();
    let mut native =
        crate::resources::test_support::FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    native.shutdown = SourceShutdown::Unconfirmed;
    let mut owner = SettlingPreparation { native, closing: Some(closing), settled };
    let returned = AtomicBool::new(false);
    let preparation = async {
        let result = prepare_owned(&mut owner, "fixture.invalid", 1433).await;
        returned.store(true, Ordering::Release);
        result
    };
    let settlement = async {
        close_started.await.unwrap();
        assert!(!returned.load(Ordering::Acquire));
        release.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(preparation, settlement);
    assert_eq!(CollectionStop::from_error(&result.err().unwrap()), CollectionStop::CancellationUnconfirmed);
    assert!(returned.load(Ordering::Acquire));
    assert_eq!(owner.native.queries(), 1);
    assert_eq!(owner.native.closed, 1);
}
