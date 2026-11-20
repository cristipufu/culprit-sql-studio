use super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    read::{Parameter, ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::{Error, ErrorCode, SourceState, source_id};
use std::sync::{
    Mutex,
    atomic::{AtomicU32, Ordering},
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Cleanup },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}
fn authority() -> Rec {
    crate::activity::database_context::tests::row(3)
}
fn replies(adr: bool) -> Vec<ReadResult<Vec<Rec>>> {
    let mut replies = vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority()]),
        Ok(vec![crate::open_transactions::tests::row()]),
        Ok(vec![crate::version_store::tests::row()]),
        Ok(vec![crate::version_store::persistent::tests::layout(3, 16, 1, 1)]),
        Ok(vec![Rec::of(&[("adr", Val::Int(i64::from(adr)))])]),
    ];
    if adr {
        replies.push(Ok(vec![crate::version_store::persistent::tests::row()]));
    }
    replies.extend([
        Ok(vec![Rec::of(&[
            ("observed_at", Val::Text("2020-01-01T06:00:01Z".into())),
            ("metadata_visibility", Val::Int(1)),
        ])]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![replace(&authority(), &[("observed_at", Val::Text("2020-01-01T06:00:02Z".into()))])]),
    ]);
    replies
}
#[derive(Default)]
struct Trace {
    sql: Mutex<Vec<String>>,
    closed: AtomicU32,
}
struct Reader {
    inner: FakeRead,
    trace: Arc<Trace>,
    generation: Option<u64>,
    change_at: Option<u32>,
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.trace.sql.lock().unwrap().push(sql.into());
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation.map(|generation| generation + u64::from(self.change_at.is_some_and(|at| self.queries() >= at)))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.trace.closed.fetch_add(1, Ordering::SeqCst);
        self.inner.close().await
    }
}
fn source(replies: Vec<ReadResult<Vec<Rec>>>, request: &CollectionRequest) -> (CleanupSource<Reader>, Arc<Trace>) {
    source_with_owner(replies, request, Some(1), None)
}
fn source_with_owner(
    replies: Vec<ReadResult<Vec<Rec>>>,
    request: &CollectionRequest,
    generation: Option<u64>,
    change_at: Option<u32>,
) -> (CleanupSource<Reader>, Arc<Trace>) {
    source_from_io(FakeRead::new(replies), request, generation, change_at)
}
fn source_from_io(
    inner: FakeRead,
    request: &CollectionRequest,
    generation: Option<u64>,
    change_at: Option<u32>,
) -> (CleanupSource<Reader>, Arc<Trace>) {
    let trace = Arc::new(Trace::default());
    let io = Reader {
        inner: inner.with_query_limit(request.budget.max_queries),
        trace: trace.clone(),
        generation,
        change_at,
    };
    (CleanupSource::with_io(io, request, "fixture.invalid".into(), 1433).unwrap(), trace)
}
async fn drain(source: &mut CleanupSource<Reader>) -> Vec<TelemetryRecord> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("bounded complete cleanup source")
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            return rows;
        }
    }
}

#[tokio::test]
async fn original_cleanup_reader_keeps_local_authority_and_exact_adr_off_on_dispatch_totals() {
    for adr in [false, true] {
        let (mut source, trace) = source(replies(adr), &request());
        let description = source.describe().await.unwrap();
        assert!(NativeReadContext::from_snapshot(&description).unwrap().has_qualified_local_owner());
        assert_eq!(description.target.database().unwrap().incarnation, None);
        assert_eq!(description.continuity, SourceContinuity::Unverified);
        let snapshot = description.observation.snapshot().unwrap();
        assert_eq!(snapshot.sources.len(), 3);
        assert_eq!(snapshot.sources[2].state, if adr { SourceState::Collected } else { SourceState::NotApplicable });
        assert_eq!(drain(&mut source).await.len(), if adr { 3 } else { 2 });
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), if adr { 12 } else { 11 });
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
        let sql = trace.sql.lock().unwrap();
        assert_eq!(sql.iter().filter(|sql| sql.contains("sys.dm_tran_version_store_space_usage")).count(), 1);
        assert_eq!(sql.iter().filter(|sql| sql.contains("sys.dm_tran_database_transactions")).count(), 1);
        assert!(sql.iter().all(|sql| !sql.contains("sys.dm_user_db_resource_governance")));
    }
}

#[tokio::test]
async fn a_completed_family_error_keeps_independent_facts_and_a_fence_stops_followup_sql() {
    let mut denied = replies(false);
    denied[3] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "denied")));
    let (mut source, _) = source(denied, &request());
    let description = source.describe().await.unwrap();
    let native = description.observation.snapshot().unwrap();
    let horizon = native.sources.iter().find(|source| source.source == source_id::OPEN_TRANSACTIONS).unwrap();
    assert_eq!(horizon.state, SourceState::Unavailable);
    assert_eq!(horizon.error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(drain(&mut source).await.len(), 1);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
    ] {
        let mut stopped = replies(false);
        stopped[3] = Err(ReadError::Stopped(stop));
        let mut io = FakeRead::new(stopped);
        let shutdown = if stop == CollectionStop::CancellationUnconfirmed {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        };
        io.shutdown = shutdown;
        let (mut source, trace) = source_from_io(io, &request(), Some(1), None);
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 4);
        assert_eq!(trace.sql.lock().unwrap().len(), 4);
        assert_eq!(source.close().await, shutdown);
    }
}

#[tokio::test]
async fn missing_native_owner_or_role_cannot_dispatch_cleanup_inventory() {
    for (field, value) in
        [("database_guid", Val::Null), ("recovery_fork_guid", Val::Null), ("updateability", Val::Null)]
    {
        let mut rows = replies(false);
        rows[2] = Ok(vec![replace(&authority(), &[(field, value)])]);
        let (mut source, trace) = source(rows, &request());
        assert!(source.describe().await.is_err(), "{field}");
        assert_eq!(source.query_count(), 3);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(trace.sql.lock().unwrap().len(), 3);
    }
    for generation in [None, Some(0)] {
        let (mut source, _) = source_with_owner(replies(false), &request(), generation, None);
        assert_eq!(
            CollectionStop::from_error(&source.describe().await.unwrap_err()),
            CollectionStop::SourceUnavailable
        );
        assert_eq!(source.query_count(), 3);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn final_database_process_role_and_socket_changes_withhold_completion() {
    for (field, value) in [
        ("database_guid", Val::Text("replacement".into())),
        ("database_file_guid", Val::Text("replacement".into())),
        ("recovery_fork_guid", Val::Text("replacement".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
    ] {
        let mut rows = replies(false);
        rows[10] = Ok(vec![replace(
            &authority(),
            &[(field, value), ("observed_at", Val::Text("2020-01-01T06:00:02Z".into()))],
        )]);
        let (mut source, _) = source(rows, &request());
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed, "{field}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let (mut changed, _) = source_with_owner(replies(false), &request(), Some(1), Some(9));
    changed.describe().await.unwrap();
    drain(&mut changed).await;
    assert!(!matches!(changed.finish().await, Ok(SourceCompletion::Verified)));
    assert_eq!(changed.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn backwards_native_boundary_and_partial_horizon_keep_their_distinct_meanings() {
    let mut rows = replies(false);
    rows[7] = Ok(vec![Rec::of(&[
        ("observed_at", Val::Text("2019-01-01T06:00:00Z".into())),
        ("metadata_visibility", Val::Int(1)),
    ])]);
    let (mut source, _) = source(rows, &request());
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut rows = replies(false);
    rows[3] = Ok(vec![replace(
        &crate::open_transactions::tests::row(),
        &[("confirmed", Val::Int(0)), ("snapshot_isolation", Val::Int(0))],
    )]);
    let (mut source, _) = self::source(rows, &request());
    let description = source.describe().await.unwrap();
    assert_eq!(description.observation.snapshot().unwrap().sources[0].state, SourceState::Partial);
    assert_eq!(drain(&mut source).await.len(), 2);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn caller_query_row_and_page_limits_are_not_replaced_by_the_native_envelope() {
    let requested = CollectionBudget {
        page_rows: 1,
        page_bytes: 4096,
        total_bytes: 8192,
        max_pages: 2,
        max_queries: 10,
        max_snapshot_rows: 2,
        statement_timeout_ms: 500,
        total_timeout_ms: 2000,
        ..Default::default()
    };
    assert_eq!(bounded_budget(&requested), requested);
    assert_eq!(bounded_budget(&CollectionBudget::upper_limits()).max_queries, MAX_QUERIES);
    let mut limited = request();
    limited.budget.max_queries = 10;
    let (mut source, _) = source(replies(false), &limited);
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 10);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut limited = request();
    limited.budget.max_snapshot_rows = 1;
    let (mut source, _) = self::source(replies(false), &limited);
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::RowLimit);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn limited_session_visibility_keeps_positive_facts_partial_and_final_visibility_loss_withholds_completion() {
    let mut rows = replies(false);
    for index in [2, 10] {
        let at = if index == 2 { "2020-01-01T06:00:00Z" } else { "2020-01-01T06:00:02Z" };
        rows[index] =
            Ok(vec![replace(&authority(), &[("database_wide", Val::Int(0)), ("observed_at", Val::Text(at.into()))])]);
    }
    let (mut source, _) = source(rows, &request());
    let description = source.describe().await.unwrap();
    let coverage = &description.observation.snapshot().unwrap().sources[0];
    assert_eq!(coverage.state, SourceState::Partial);
    assert!(!coverage.complete_scope);
    assert_eq!(drain(&mut source).await.len(), 2, "positive native facts survive incomplete visibility");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut rows = replies(false);
    rows[10] = Ok(vec![replace(
        &authority(),
        &[("database_wide", Val::Int(0)), ("observed_at", Val::Text("2020-01-01T06:00:02Z".into()))],
    )]);
    let (mut source, _) = self::source(rows, &request());
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn azure_database_profile_keeps_its_existing_operational_route_and_is_not_an_instance_cleanup_alias() {
    let platform =
        replace(&metadata(), &[("edition", Val::Int(5)), ("machine_name", Val::Null), ("process_id", Val::Null)]);
    let azure = crate::activity::database_context::tests::row(5);
    let (mut source, trace) = source(vec![Ok(vec![platform]), Ok(vec![azure])], &request());
    assert_eq!(source.describe().await.unwrap_err().code, ErrorCode::Unsupported);
    assert_eq!(source.query_count(), 2);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(trace.sql.lock().unwrap().iter().all(|sql| !sql.contains("sys.dm_tran_database_transactions")));
}
