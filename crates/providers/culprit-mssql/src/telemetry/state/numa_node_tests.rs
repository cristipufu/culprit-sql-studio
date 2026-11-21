use super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::SourceState;
struct OriginalRead {
    inner: FakeRead,
    generation: Option<u64>,
    change_at: Option<usize>,
}
#[async_trait::async_trait]
impl NativeRead for OriginalRead {
    async fn query(
        &mut self,
        sql: &str,
        parameters: Vec<crate::read::Parameter>,
        limit: usize,
    ) -> crate::read::ReadResult<Vec<crate::value::Rec>> {
        let result = self.inner.query(sql, parameters, limit).await;
        if self.change_at == Some(self.inner.calls.len()) {
            self.generation = Some(2)
        }
        result
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}
fn request(total: u32) -> CollectionRequest {
    let mut request = super::shared_tests::request();
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::NumaNodes };
    request.budget.max_queries = total;
    request
}
fn source(total: u32, deny_cpu: bool, changed: bool) -> StateSource<OriginalRead> {
    source_with_final_clock(total, deny_cpu, changed, "2020-01-01T06:02:01Z")
}
fn source_with_final_clock(total: u32, deny_cpu: bool, changed: bool, final_at: &str) -> StateSource<OriginalRead> {
    let cpu = MssqlNumaNodeKind::Cpu;
    let memory = MssqlNumaNodeKind::Memory;
    let mut replies =
        vec![Ok(vec![metadata()]), Ok(vec![epoch(100)]), Ok(vec![super::tests::clock(false, "2020-01-01T06:02:00Z")])];
    if deny_cpu {
        replies.push(Err(crate::read::ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "fixture denial",
        ))))
    } else {
        replies.extend([Ok(crate::numa_nodes::tests::layout(cpu)), Ok(vec![crate::numa_nodes::tests::row(cpu)])]);
    }
    replies.extend([
        Ok(crate::numa_nodes::tests::layout(memory)),
        Ok(vec![crate::numa_nodes::tests::row(memory)]),
        Ok(vec![super::tests::clock(false, "2020-01-01T06:02:01Z")]),
        Ok(vec![replace(
            &metadata(),
            &[("process_id", Val::Int(if changed { 43 } else { 42 })), ("observed_at", Val::Text(final_at.into()))],
        )]),
        Ok(vec![epoch(100)]),
    ]);
    let native = ReadSession::budget_after_setup(numa_nodes_budget(&request(total).budget)).unwrap();
    StateSource::with_io(
        OriginalRead {
            inner: FakeRead::new(replies).with_query_limit(native.max_queries),
            generation: Some(1),
            change_at: None,
        },
        &request(total),
        "fixture.invalid".into(),
        1433,
    )
    .unwrap()
}
async fn drain(source: &mut StateSource<OriginalRead>) -> Vec<TelemetryRecord> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    rows
}
#[tokio::test]
async fn explicit_node_profile_pays_ten_native_reads_plus_setup_and_preserves_two_original_populations() {
    let mut source = source(11, false, false);
    let description = source.describe().await.unwrap();
    let rows = drain(&mut source).await;
    assert!(numa_node_header_is_valid(&description));
    assert_eq!(rows.len(), 2);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count() + ReadSession::SESSION_SETUP_QUERIES, 11);
    assert!(source.io.inner.calls.iter().all(|call| call.sql.contains("/* culprit */")));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn one_denied_family_preserves_the_other_and_tighter_total_withholds_final_verification() {
    let mut denied = source(11, true, false);
    let description = denied.describe().await.unwrap();
    let rows = drain(&mut denied).await;
    assert_eq!(rows.len(), 1);
    let sources = &description.observation.snapshot().unwrap().sources;
    assert_eq!(sources[0].state, SourceState::Unavailable);
    assert_eq!(sources[1].state, SourceState::Collected);
    assert_eq!(denied.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(denied.close().await, SourceShutdown::Quiescent);
    let mut tight = source(10, false, false);
    tight.describe().await.unwrap();
    drain(&mut tight).await;
    assert_eq!(CollectionStop::from_error(&tight.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(tight.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn process_or_socket_reuse_cannot_verify_and_unknown_socket_stops_before_node_work() {
    let mut process = source(11, false, true);
    process.describe().await.unwrap();
    drain(&mut process).await;
    assert_eq!(process.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(process.close().await, SourceShutdown::Quiescent);
    let mut socket = source(11, false, false);
    socket.io.change_at = Some(9);
    socket.describe().await.unwrap();
    drain(&mut socket).await;
    assert_eq!(socket.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(socket.close().await, SourceShutdown::Quiescent);
    let mut unknown = source(11, false, false);
    unknown.io.generation = None;
    assert_eq!(CollectionStop::from_error(&unknown.describe().await.unwrap_err()), CollectionStop::SourceUnavailable);
    assert_eq!(unknown.query_count(), 2);
    assert_eq!(unknown.close().await, SourceShutdown::Quiescent);
}
#[test]
fn node_recipe_does_not_accept_database_or_pool_and_intersects_every_caller_limit() {
    for subject in [
        CollectionSubject::Database,
        CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Pool,
                namespace: NativeIdentity::new("pool-space"),
                native: NativeIdentity::new("pool"),
            },
        },
    ] {
        let mut request = request(11);
        request.subject = subject;
        assert!(
            StateSource::with_io(
                OriginalRead { inner: FakeRead::new(vec![]), generation: Some(1), change_at: None },
                &request,
                "fixture.invalid".into(),
                1433
            )
            .is_err()
        );
    }
    let caller = CollectionBudget {
        max_queries: 7,
        page_rows: 2,
        page_bytes: 4096,
        total_bytes: 8192,
        max_snapshot_rows: 3,
        statement_timeout_ms: 500,
        total_timeout_ms: 2000,
        ..Default::default()
    };
    assert_eq!(numa_nodes_budget(&caller), caller);
    assert_eq!(numa_nodes_budget(&CollectionBudget::upper_limits()).max_queries, 11);
}

#[tokio::test]
async fn final_native_clock_must_not_precede_the_last_population_boundary() {
    for (at, expected) in [
        ("2020-01-01T06:02:01Z", SourceCompletion::Verified),
        ("2020-01-01T06:02:02Z", SourceCompletion::Verified),
        ("2020-01-01T06:02:00.9999999Z", SourceCompletion::Changed),
    ] {
        let mut source = source_with_final_clock(11, false, false, at);
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert_eq!(source.finish().await.unwrap(), expected, "{at}");
        assert_eq!(source.query_count(), 10);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn cancelled_node_metadata_read_stops_before_the_other_population_and_still_retires_its_original_reader() {
    let mut source = source(11, false, false);
    source.io.inner = FakeRead::new(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![super::tests::clock(false, "2020-01-01T06:02:00Z")]),
        Err(crate::read::ReadError::Stopped(CollectionStop::Cancelled)),
    ]);
    source.io.inner.shutdown = SourceShutdown::Unconfirmed;
    let error = source.describe().await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(source.io.inner.closed, 1);
}
