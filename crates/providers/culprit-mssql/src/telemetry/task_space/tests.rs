use super::*;
use crate::{
    activity::{page_targets::tests::participant, tempdb::tests::task, tests::details},
    instance::test_support::{epoch, metadata},
    read::{Parameter, ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use std::sync::{
    Mutex,
    atomic::{AtomicU32, Ordering},
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::TaskSpace },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}
fn platform(edition: i64, major: i64) -> Rec {
    replace(&metadata(), &[("edition", Val::Int(edition)), ("major", Val::Int(major))])
}
fn authority(edition: i64, major: i64) -> Rec {
    let row = crate::activity::database_context::tests::row(edition);
    let mut fields = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.push(("task_space_permission", Val::Int(1)));
    replace(&Rec::of(&fields), &[("major", Val::Int(major))])
}
fn frame(edition: i64) -> Vec<Rec> {
    [task(0), task(1)].into_iter().map(|row| replace(&row, &[("edition", Val::Int(edition))])).collect()
}
fn replies(edition: i64, major: i64) -> Vec<ReadResult<Vec<Rec>>> {
    let owner = || {
        if edition == 5 {
            vec![Ok(vec![authority(edition, major)])]
        } else {
            vec![Ok(vec![platform(edition, major)]), Ok(vec![epoch(100)]), Ok(vec![authority(edition, major)])]
        }
    };
    let check = participant(0, &task(0));
    let mut rows = vec![Ok(vec![platform(edition, major)])];
    if edition != 5 {
        rows.clear();
    }
    rows.extend(owner());
    rows.extend([
        Ok(frame(edition)),
        Ok(vec![check.clone()]),
        Ok(vec![details(42, "SELECT value FROM dbo.orders WHERE id=42")]),
        Ok(vec![check]),
    ]);
    rows.extend(owner());
    rows.extend(owner());
    rows
}
#[derive(Default)]
struct Trace {
    queries: Mutex<Vec<String>>,
    closed: AtomicU32,
}
struct SourceRead {
    inner: FakeRead,
    trace: Arc<Trace>,
    generation: Option<u64>,
    change_generation_at: Option<u32>,
}
#[async_trait::async_trait]
impl NativeRead for SourceRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.trace.queries.lock().unwrap().push(sql.into());
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation
            .map(|generation| generation + u64::from(self.change_generation_at.is_some_and(|at| self.queries() >= at)))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.trace.closed.fetch_add(1, Ordering::SeqCst);
        self.inner.close().await
    }
}
fn source(io: FakeRead, request: &CollectionRequest) -> (TaskSpaceSource<SourceRead>, Arc<Trace>) {
    source_with_generation(io, request, Some(1), None)
}
fn source_with_generation(
    io: FakeRead,
    request: &CollectionRequest,
    generation: Option<u64>,
    change_at: Option<u32>,
) -> (TaskSpaceSource<SourceRead>, Arc<Trace>) {
    let trace = Arc::new(Trace::default());
    let reader = SourceRead { inner: io, trace: trace.clone(), generation, change_generation_at: change_at };
    (TaskSpaceSource::with_io(reader, request, "fixture.invalid".into(), 1433).unwrap(), trace)
}
async fn drain(source: &mut TaskSpaceSource<SourceRead>) -> Vec<TelemetryRecord> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("finite complete source")
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            return rows;
        }
    }
}

#[tokio::test]
async fn boxed_factory_pages_reuse_the_shared_native_owner_without_global_database_authority() {
    let (mut source, trace) = source(FakeRead::new(replies(3, 16)), &request());
    let description = source.describe().await.unwrap();
    assert!(
        matches!(&description.partition,SourcePartition::InstanceLocal {member:Some(member),epoch:Some(_)} if member.kind==SharedResourceKind::Instance)
    );
    assert!(description.target.database().unwrap().incarnation.is_none());
    assert_eq!(source.query_count(), 10);
    let rows = drain(&mut source).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(TelemetryRecord::is_valid));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), crate::task_space::MAX_BOXED_QUERIES);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    assert!(trace.queries.lock().unwrap().iter().all(|sql| !sql.contains("sys.dm_user_db_resource_governance")));
    let queries = trace.queries.lock().unwrap();
    for index in [2, 9, 12] {
        assert!(queries[index].contains("sys.database_recovery_status"));
        assert!(queries[index].contains(" AS task_space_permission"));
        assert_eq!(queries[index].matches(crate::activity::tempdb::TASK_SPACE_PERMISSION).count(), 1);
    }
}

#[tokio::test]
async fn azure_factory_retains_native_identity_and_primary_admission_before_task_inventory() {
    for role in [Some("READ_WRITE"), Some("READ_ONLY"), None] {
        let mut rows = replies(5, 16);
        rows[1] = Ok(vec![replace(
            &authority(5, 16),
            &[("updateability", role.map(|role| Val::Text(role.into())).unwrap_or(Val::Null))],
        )]);
        let (mut source, trace) = source(FakeRead::new(rows), &request());
        let description = source.describe().await;
        if role == Some("READ_WRITE") {
            let description = description.unwrap();
            assert_eq!(description.target.database().unwrap().incarnation.as_deref(), Some("logical-guid"));
            assert!(description.partition.is_qualified());
            assert_eq!(drain(&mut source).await.len(), 2);
            assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
            assert_eq!(source.query_count(), crate::task_space::MAX_AZURE_QUERIES);
            let queries = trace.queries.lock().unwrap();
            for index in [1, 6, 7] {
                assert!(queries[index].contains("sys.dm_user_db_resource_governance"));
                assert!(queries[index].contains(" AS task_space_permission"));
                assert!(!queries[index].contains("sys.database_recovery_status"));
            }
        } else {
            assert!(description.is_err());
            assert_eq!(source.query_count(), 2);
            assert!(trace.queries.lock().unwrap().iter().all(|sql| !sql.contains("FROM sys.dm_db_task_space_usage")));
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn unsupported_native_profiles_permissions_and_missing_socket_proof_refuse_before_task_inventory() {
    for (edition, major) in [(3, 15), (8, 16), (6, 16)] {
        let (mut source, trace) = source(FakeRead::new(replies(edition, major)), &request());
        assert_eq!(source.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
        assert!(trace.queries.lock().unwrap().iter().all(|sql| !sql.contains("FROM sys.dm_db_task_space_usage")));
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut rows = replies(3, 16);
    rows[2] = Ok(vec![replace(&authority(3, 16), &[("task_space_permission", Val::Int(0))])]);
    let (mut denied, trace) = source(FakeRead::new(rows), &request());
    assert_eq!(denied.describe().await.unwrap_err().code, culprit_core::ErrorCode::PermissionDenied);
    assert_eq!(denied.query_count(), 3);
    assert_eq!(denied.close().await, SourceShutdown::Quiescent);
    assert!(trace.queries.lock().unwrap().iter().all(|sql| !sql.contains("FROM sys.dm_db_task_space_usage")));
    for (field, value) in
        [("updateability", Val::Text("READ_ONLY".into())), ("database_guid", Val::Null), ("database_wide", Val::Int(0))]
    {
        let mut rows = replies(3, 16);
        rows[2] = Ok(vec![replace(&authority(3, 16), &[(field, value)])]);
        let (mut unavailable, trace) = source(FakeRead::new(rows), &request());
        assert!(unavailable.describe().await.is_err(), "{field}");
        assert_eq!(unavailable.query_count(), 3);
        assert!(trace.queries.lock().unwrap().iter().all(|sql| !sql.contains("FROM sys.dm_db_task_space_usage")));
        assert_eq!(unavailable.close().await, SourceShutdown::Quiescent);
    }
    for generation in [None, Some(0)] {
        let (mut unknown, _) = source_with_generation(FakeRead::new(replies(3, 16)), &request(), generation, None);
        assert_eq!(
            CollectionStop::from_error(&unknown.describe().await.unwrap_err()),
            CollectionStop::SourceUnavailable
        );
        assert_eq!(unknown.query_count(), 3);
        assert_eq!(unknown.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn original_database_instance_role_clock_and_connection_changes_cannot_complete_capture() {
    let mut rows = replies(5, 16);
    rows[2] = Ok(frame(5)
        .iter()
        .map(|row| replace(row, &[("incarnation", Val::Text("foreign-logical-guid".into()))]))
        .collect());
    let (mut foreign_frame, trace) = source(FakeRead::new(rows), &request());
    assert_eq!(CollectionStop::from_error(&foreign_frame.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(foreign_frame.query_count(), 3);
    assert_eq!(foreign_frame.close().await, SourceShutdown::Quiescent);
    assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    for (field, value) in [
        ("selected_database_id", Val::Int(8)),
        ("database_guid", Val::Text("replacement-database".into())),
        ("database_file_guid", Val::Text("replacement-file".into())),
        ("recovery_fork_guid", Val::Text("replacement-fork".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("observed_at", Val::Text("2020-01-01T05:59:59Z".into())),
    ] {
        let mut rows = replies(3, 16);
        rows[9] = Ok(vec![replace(&authority(3, 16), &[(field, value)])]);
        let (mut changed, _) = source(FakeRead::new(rows), &request());
        assert!(changed.describe().await.is_err(), "{field}");
        assert_eq!(changed.query_count(), 10);
        assert_eq!(changed.close().await, SourceShutdown::Quiescent);
    }
    let mut rows = replies(3, 16);
    rows[8] = Ok(vec![epoch(101)]);
    let (mut changed, _) = source(FakeRead::new(rows), &request());
    assert!(changed.describe().await.is_err());
    assert_eq!(changed.close().await, SourceShutdown::Quiescent);
    let mut rows = replies(3, 16);
    rows[9] =
        Ok(vec![replace(&authority(3, 16), &[("observed_at", Val::Text("2020-01-01T06:00:00.000000100Z".into()))])]);
    rows[12] =
        Ok(vec![replace(&authority(3, 16), &[("observed_at", Val::Text("2020-01-01T06:00:00.000000099Z".into()))])]);
    let (mut backwards, _) = source(FakeRead::new(rows), &request());
    backwards.describe().await.unwrap();
    drain(&mut backwards).await;
    assert_eq!(backwards.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(backwards.close().await, SourceShutdown::Quiescent);
    let (mut recycled, _) = source_with_generation(FakeRead::new(replies(3, 16)), &request(), Some(1), Some(13));
    recycled.describe().await.unwrap();
    drain(&mut recycled).await;
    assert!(recycled.finish().await.is_err());
    assert_eq!(recycled.query_count(), 13);
    assert_eq!(recycled.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn cancelled_failed_and_stricter_caller_reads_keep_the_original_close_owner_without_fallback() {
    for shutdown in [SourceShutdown::Quiescent, SourceShutdown::Unconfirmed] {
        let mut io = FakeRead::new(replies(3, 16)).pause_on_query(4);
        io.shutdown = shutdown;
        let (mut source, trace) = source(io, &request());
        assert!(tokio::time::timeout(std::time::Duration::from_millis(1), source.describe()).await.is_err());
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, shutdown);
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
        assert!(source.finish().await.is_err());
        assert_eq!(source.query_count(), 4);
    }
    let mut limited_request = request();
    limited_request.budget.max_queries = 3;
    let (mut limited, trace) = source(FakeRead::new(replies(3, 16)).with_query_limit(3), &limited_request);
    assert_eq!(CollectionStop::from_error(&limited.describe().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(limited.query_count(), 3);
    assert_eq!(limited.close().await, SourceShutdown::Quiescent);
    assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    let mut capped_request = request();
    capped_request.budget.max_snapshot_rows = 1;
    let (mut capped, _) = source(FakeRead::new(replies(3, 16)), &capped_request);
    let description = capped.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 1);
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    assert_eq!(drain(&mut capped).await.len(), 1);
    assert_eq!(capped.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(capped.query_count(), crate::task_space::MAX_BOXED_QUERIES);
    assert_eq!(capped.close().await, SourceShutdown::Quiescent);
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let mut rows = replies(3, 16);
        rows[3] = Err(ReadError::Stopped(stop));
        let (mut failed, trace) = source(FakeRead::new(rows), &request());
        assert_eq!(CollectionStop::from_error(&failed.describe().await.unwrap_err()), stop);
        assert_eq!(failed.query_count(), 4);
        assert_eq!(failed.close().await, SourceShutdown::Quiescent);
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn wrong_or_focused_selections_and_expanded_subjects_do_not_enter_the_task_factory() {
    for selection in [
        CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory },
        CollectionSelection::FocusedRequestMemory {
            focus: Box::new(crate::activity::memory::focus::tests::original_focus()),
        },
    ] {
        let mut wrong = request();
        wrong.selection = selection;
        assert!(TaskSpaceSource::with_io(FakeRead::new(vec![]), &wrong, "unused.invalid".into(), 1).is_err());
    }
    let mut shared = request();
    shared.subject = CollectionSubject::Shared {
        resource: SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native-namespace"),
            native: NativeIdentity::new("native-instance"),
        },
    };
    assert!(TaskSpaceSource::with_io(FakeRead::new(vec![]), &shared, "unused.invalid".into(), 1).is_err());
    let bounded = bounded_request(&request()).unwrap().budget;
    assert_eq!(
        (bounded.max_snapshot_rows, bounded.statement_timeout_ms, bounded.total_timeout_ms, bounded.max_queries),
        (200, 1000, 5000, 13)
    );
    let recipe = crate::telemetry::recipes().into_iter().find(|recipe| recipe.id == "mssql.task-space").unwrap();
    assert_eq!(recipe.version, 3);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
}
