use super::*;
use culprit_collector::analytical::test_support::baseline_profile_expectation;
use culprit_read::SourceProfileGuard;
use std::sync::atomic::{AtomicBool, Ordering};
struct ProfiledRead {
    original: FakeRead,
    profile: SourceProfileGuard,
    active: Arc<AtomicBool>,
    retire_after: Option<usize>,
    generation: u64,
    change_connection_after: Option<usize>,
}
#[async_trait::async_trait]
impl NativeRead for ProfiledRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.profile.check_connection(Some(self.generation)).map_err(ReadError::Source)?;
        let result = self.original.query(sql, parameters, limit).await;
        if self.retire_after == Some(self.original.calls.len()) {
            self.active.store(false, Ordering::Release);
        }
        if self.change_connection_after == Some(self.original.calls.len()) {
            self.generation += 1;
        }
        result
    }
    fn queries(&self) -> u32 {
        self.original.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(self.generation)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.original.close().await
    }
}
fn context(writable: bool) -> NativeReadContext {
    let row = authority(&replace(
        &metadata(0),
        &[
            ("observed_at", Val::Text("2020-01-01T05:59:59Z".into())),
            ("updateability", Val::Text(if writable { "READ_WRITE" } else { "READ_ONLY" }.into())),
        ],
    ));
    let context = database_context::test_azure_authority(&row).unwrap();
    context.completed(&context).unwrap()
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: crate::telemetry::activity::bounded_budget(&CollectionBudget::default()),
    }
}
fn profiled(
    expected: NativeReadContext,
    writable: bool,
) -> (ActivityReader<ProfiledRead>, SourceProfileGuard, Arc<AtomicBool>) {
    let active = Arc::new(AtomicBool::new(true));
    let flag = active.clone();
    let admission = baseline_profile_expectation(&request(), &expected, move || flag.load(Ordering::Acquire));
    let profile = SourceProfileGuard::new(&request(), &admission).unwrap();
    let native = replace(
        &metadata(42),
        &[("updateability", Val::Text(if writable { "READ_WRITE" } else { "READ_ONLY" }.into()))],
    );
    let original = baseline_reader(vec![native]).io;
    // The existing factory's authority bounds must retain that same known native role.
    let mut original = original;
    for index in [0, 2] {
        let row = original.replies[index].as_ref().unwrap()[0].clone();
        original.replies[index] = Ok(vec![replace(
            &row,
            &[("updateability", Val::Text(if writable { "READ_WRITE" } else { "READ_ONLY" }.into()))],
        )]);
    }
    let io = ProfiledRead {
        original,
        profile: profile.clone(),
        active: active.clone(),
        retire_after: None,
        generation: 1,
        change_connection_after: None,
    };
    (
        ActivityReader::new(
            io,
            ReadOptions { budget: request().budget, identity: IdentityDetail::AzureDatabase },
            "fixture.invalid".into(),
            1433,
        ),
        profile,
        active,
    )
}
#[tokio::test]
async fn exact_profile_reuses_the_original_three_reads_and_preserves_writable_or_readonly_native_role() {
    for writable in [true, false] {
        let (mut reader, profile, _) = profiled(context(writable), writable);
        let pending = capture::PendingActivityFrame::begin_profiled_census(&mut reader, &profile).await.unwrap();
        let completed = pending.finish(&mut reader).await.unwrap();
        assert_eq!(completed.description.observation.activity().unwrap().database_writable, Some(writable));
        assert_eq!(reader.query_count(), 3);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(reader.io.original.closed, 1);
    }
}
#[tokio::test]
async fn a_foreign_native_profile_or_unknown_role_stops_before_activity_population() {
    for field in ["database", "member", "epoch", "role", "unknown-role"] {
        let mut expected = context(true);
        match field {
            "database" => {
                let CollectionTarget::Database(db) = &mut expected.target else { unreachable!() };
                db.incarnation = Some("foreign-original-logical-db".into());
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut expected.partition else {
                    unreachable!()
                };
                member.native.id = "foreign-original-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut expected.partition else { unreachable!() };
                *epoch = Some("foreign-original-epoch".into());
            }
            "role" => expected.database_writable = Some(false),
            _ => (),
        }
        let (mut reader, profile, _) = profiled(expected, true);
        if field == "unknown-role" {
            let row = reader.io.original.replies[0].as_ref().unwrap()[0].clone();
            reader.io.original.replies[0] = Ok(vec![replace(&row, &[("updateability", Val::Null)])]);
        }
        let error = capture::PendingActivityFrame::begin_profiled_census(&mut reader, &profile).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(reader.query_count(), 1, "{field}: no population or final query");
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn retirement_between_authority_population_or_final_work_never_commits_or_heals_on_reactivation() {
    for retire_after in [1, 2, 3] {
        let (mut reader, profile, active) = profiled(context(true), true);
        reader.io.retire_after = Some(retire_after);
        let result = async {
            capture::PendingActivityFrame::begin_profiled_census(&mut reader, &profile).await?.finish(&mut reader).await
        }
        .await;
        assert_eq!(CollectionStop::from_error(&result.err().unwrap()), CollectionStop::SourceChanged);
        assert_eq!(reader.query_count(), retire_after as u32);
        active.store(true, Ordering::Release);
        assert!(profile.ensure_active().is_err());
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(reader.io.original.closed, 1);
    }
}
#[tokio::test]
async fn a_retired_held_connection_is_not_replaced_before_the_next_internal_dispatch() {
    let (mut reader, profile, _) = profiled(context(true), true);
    reader.io.change_connection_after = Some(2);
    let pending = capture::PendingActivityFrame::begin_profiled_census(&mut reader, &profile).await.unwrap();
    let error = pending.finish(&mut reader).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(reader.query_count(), 2);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn an_unconfirmed_original_close_is_not_healed_by_profile_retirement() {
    let (mut reader, profile, _) = profiled(context(true), true);
    reader.io.retire_after = Some(2);
    reader.io.original.shutdown = SourceShutdown::Unconfirmed;
    let pending = capture::PendingActivityFrame::begin_profiled_census(&mut reader, &profile).await.unwrap();
    assert!(pending.finish(&mut reader).await.is_err());
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(reader.io.original.closed, 1);
}
#[tokio::test]
async fn retired_builtin_profile_starts_no_authentication_setup_or_native_source_work() {
    use culprit_core::Provider;
    let active = Arc::new(AtomicBool::new(true));
    let flag = active.clone();
    let admission = baseline_profile_expectation(&request(), &context(true), move || flag.load(Ordering::Acquire));
    let provider = crate::MssqlProvider::new(&culprit_core::ConnectionSpec {
        engine: Some(Engine::Mssql),
        host: "no-native-dispatch.invalid".into(),
        database: Some("shop".into()),
        user: Some("monitor".into()),
        ..Default::default()
    })
    .unwrap();
    let mut source = provider.telemetry_source_for_profile(&request(), &admission).unwrap();
    active.store(false, Ordering::Release);
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[test]
fn retirement_prevents_the_actual_owned_setup_recorder_from_charging_or_dispatching_set() {
    let active = Arc::new(AtomicBool::new(true));
    let flag = active.clone();
    let admission = baseline_profile_expectation(&request(), &context(true), move || flag.load(Ordering::Acquire));
    let profile = SourceProfileGuard::new(&request(), &admission).unwrap();
    let setup = crate::read::qualification::SetupWork::for_profile(profile);
    active.store(false, Ordering::Release);
    assert!(setup.begin("SET LOCK_TIMEOUT 2000").is_err());
    assert_eq!(setup.snapshot().0, 0);
}

#[tokio::test]
async fn retirement_while_resolving_the_route_stops_before_tcp_login_or_set_without_losing_close_ownership() {
    use culprit_core::{ConnectionEndpoint, ConnectionTransport, Provider};
    use std::sync::atomic::AtomicUsize;
    struct RetiringRoute {
        active: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ConnectionTransport for RetiringRoute {
        async fn endpoint(&self, _: &str, _: u16) -> Result<ConnectionEndpoint> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            self.active.store(false, Ordering::Release);
            Ok(ConnectionEndpoint::new("127.0.0.1:9".parse().unwrap()))
        }
    }
    let active = Arc::new(AtomicBool::new(true));
    let flag = active.clone();
    let admission = baseline_profile_expectation(&request(), &context(true), move || flag.load(Ordering::Acquire));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = crate::MssqlProvider::new(&culprit_core::ConnectionSpec {
        engine: Some(Engine::Mssql),
        host: "no-native-dispatch.invalid".into(),
        database: Some("shop".into()),
        user: Some("monitor".into()),
        transport: Some(Arc::new(RetiringRoute { active, calls: calls.clone() })),
        ..Default::default()
    })
    .unwrap();
    let mut source = provider.telemetry_source_for_profile(&request(), &admission).unwrap();
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert!(source.read_metrics().is_none_or(|metrics| metrics.templates.is_empty()));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
