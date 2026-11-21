use super::*;
use culprit_collector::analytical::test_support::baseline_profile_expectation;
use culprit_read::SourceProfileGuard;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
struct ProfiledRead {
    original: Read,
    profile: SourceProfileGuard,
    active: Arc<AtomicBool>,
    retire_after: Option<usize>,
    generation: u64,
    change_connection_after: Option<usize>,
}
#[async_trait::async_trait]
impl QueryRead for ProfiledRead {
    fn connection_generation(&self) -> Option<u64> {
        Some(self.generation)
    }
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        self.profile.check_connection(Some(self.generation))?;
        let result = self.original.query(sql, limit).await;
        if self.retire_after == Some(self.original.calls.len()) {
            self.active.store(false, Ordering::Release);
        }
        if self.change_connection_after == Some(self.original.calls.len()) {
            self.generation += 1;
        }
        result
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: bounded_budget(&CollectionBudget::default()),
    }
}
fn native(writable: bool) -> Read {
    let mut read = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
    for (phase, at) in [(0, "2020-01-01T06:00:00Z"), (3, "2020-01-01T06:00:00.200000Z")] {
        read.replies[phase] = Ok(vec![metadata_with_role(
            at,
            "2020-01-01T05:00:00Z",
            true,
            Some(Some(if writable { "f" } else { "t" })),
        )]);
    }
    read
}
async fn context(writable: bool) -> NativeReadContext {
    let capture = capture(&mut native(writable), "fixture.invalid", 5432, &request()).await.unwrap();
    NativeReadContext::from_activity(&capture.description).unwrap()
}
fn profiled(expected: &NativeReadContext, writable: bool) -> (ProfiledRead, SourceProfileGuard, Arc<AtomicBool>) {
    let active = Arc::new(AtomicBool::new(true));
    let flag = active.clone();
    let admission = baseline_profile_expectation(&request(), expected, move || flag.load(Ordering::Acquire));
    let profile = SourceProfileGuard::new(&request(), &admission).unwrap();
    (
        ProfiledRead {
            original: native(writable),
            profile: profile.clone(),
            active: active.clone(),
            retire_after: None,
            generation: 1,
            change_connection_after: None,
        },
        profile,
        active,
    )
}
#[tokio::test]
async fn a_native_local_profile_reuses_five_reads_and_accepts_only_its_known_original_role_without_global_incarnation()
{
    for writable in [true, false] {
        let expected = context(writable).await;
        assert!(expected.target.database().unwrap().incarnation.is_none());
        let (mut reader, profile, _) = profiled(&expected, writable);
        let captured =
            capture_with_profile(&mut reader, "fixture.invalid", 5432, &request(), Some(&profile)).await.unwrap();
        assert_eq!(NativeReadContext::from_activity(&captured.description).unwrap().database_writable, Some(writable));
        assert_eq!(reader.original.calls.len(), 5);
        assert_eq!(captured.records.len(), 1);
    }
}
#[tokio::test]
async fn wrong_database_member_process_or_role_and_missing_initial_owner_stop_before_the_population_query() {
    let original = context(true).await;
    for field in ["database", "member", "epoch", "role", "missing-owner"] {
        let mut expected = original.clone();
        match field {
            "database" => {
                let CollectionTarget::Database(database) = &mut expected.target else { unreachable!() };
                database.database = "another-original-database".into();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut expected.partition else {
                    unreachable!()
                };
                member.native.id = "foreign-native-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut expected.partition else { unreachable!() };
                *epoch = Some("foreign-native-process".into());
            }
            "role" => expected.database_writable = Some(false),
            _ => (),
        }
        let (mut reader, profile, _) = profiled(&expected, true);
        if field == "missing-owner" {
            reader.original.replies[1] =
                Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.unsupported")));
        }
        let failure =
            capture_with_profile(&mut reader, "fixture.invalid", 5432, &request(), Some(&profile)).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&failure), CollectionStop::SourceChanged, "{field}");
        assert_eq!(reader.original.calls.len(), 2, "{field}: authority only, never Activity population");
    }
}
#[tokio::test]
async fn retirement_between_the_five_original_reads_or_final_check_has_no_reactivation_or_partial_commit() {
    let original = context(true).await;
    for retire_after in 1..=5 {
        let (mut reader, profile, active) = profiled(&original, true);
        reader.retire_after = Some(retire_after);
        let failure =
            capture_with_profile(&mut reader, "fixture.invalid", 5432, &request(), Some(&profile)).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&failure), CollectionStop::SourceChanged);
        assert_eq!(reader.original.calls.len(), retire_after);
        active.store(true, Ordering::Release);
        assert!(profile.ensure_active().is_err());
    }
}
#[tokio::test]
async fn held_transport_replacement_and_final_role_change_refuse_all_provisional_rows() {
    let original = context(true).await;
    let (mut reader, profile, _) = profiled(&original, true);
    reader.change_connection_after = Some(3);
    let failure =
        capture_with_profile(&mut reader, "fixture.invalid", 5432, &request(), Some(&profile)).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&failure), CollectionStop::SourceChanged);
    assert_eq!(reader.original.calls.len(), 3);
    let (mut reader, profile, _) = profiled(&original, true);
    reader.original.replies[3] =
        Ok(vec![metadata_with_role("2020-01-01T06:00:00.200000Z", "2020-01-01T05:00:00Z", true, Some(Some("t")))]);
    let failure =
        capture_with_profile(&mut reader, "fixture.invalid", 5432, &request(), Some(&profile)).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&failure), CollectionStop::SourceChanged);
    assert_eq!(reader.original.calls.len(), 4);
}

#[tokio::test]
async fn a_retired_actual_builtin_source_never_authenticates_or_runs_owned_set() {
    use culprit_core::{Engine, Provider};
    let active = Arc::new(AtomicBool::new(true));
    let flag = active.clone();
    let expected = context(true).await;
    let admission = baseline_profile_expectation(&request(), &expected, move || flag.load(Ordering::Acquire));
    let provider = crate::PostgresProvider::new(&culprit_core::ConnectionSpec {
        engine: Some(Engine::Postgres),
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

#[tokio::test]
async fn native_route_retirement_cannot_authenticate_or_send_the_pending_setup_batch() {
    use culprit_core::{ConnectionEndpoint, ConnectionTransport, Engine, Provider};
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
    let original = context(true).await;
    let admission = baseline_profile_expectation(&request(), &original, move || flag.load(Ordering::Acquire));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = crate::PostgresProvider::new(&culprit_core::ConnectionSpec {
        engine: Some(Engine::Postgres),
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
    assert_eq!(source.query_count(), 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
