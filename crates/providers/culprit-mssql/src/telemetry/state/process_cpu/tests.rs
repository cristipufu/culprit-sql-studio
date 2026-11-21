use super::super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{Engine, SourceState};
struct Original {
    inner: FakeRead,
    generation: Option<u64>,
}
#[async_trait::async_trait]
impl NativeRead for Original {
    async fn query(
        &mut self,
        sql: &str,
        parameters: Vec<crate::read::Parameter>,
        limit: usize,
    ) -> crate::read::ReadResult<Vec<crate::value::Rec>> {
        self.inner.query(sql, parameters, limit).await
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
    let mut request = super::super::shared_tests::request();
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::ProcessCpu };
    request.budget.max_queries = total;
    request
}
fn replies() -> Vec<crate::read::ReadResult<Vec<crate::value::Rec>>> {
    vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![super::super::tests::clock(false, "2020-01-01T06:00:00Z")]),
        Ok(vec![crate::processors::tests::row()]),
        Ok(vec![crate::processors::cpu_test_support::pool_row()]),
        Ok(vec![crate::processors::cpu_test_support::history_row()]),
        Ok(vec![super::super::tests::clock(false, "2020-01-01T06:00:01Z")]),
        Ok(vec![replace(&metadata(), &[("observed_at", Val::Text("2020-01-01T06:00:02Z".into()))])]),
        Ok(vec![epoch(100)]),
    ]
}
fn source(total: u32, replies: Vec<crate::read::ReadResult<Vec<crate::value::Rec>>>) -> StateSource<Original> {
    let request = request(total);
    let budget = ReadSession::budget_after_setup(process_cpu_budget(&request.budget)).unwrap();
    StateSource::with_io(
        Original { inner: FakeRead::new(replies).with_query_limit(budget.max_queries), generation: Some(1) },
        &request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap()
}
async fn drain(source: &mut StateSource<Original>) -> Vec<TelemetryRecord> {
    let mut rows = vec![];
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
async fn explicit_cpu_profile_pays_nine_original_queries_plus_setup_and_keeps_independent_native_populations() {
    let mut source = source(10, replies());
    let description = source.describe().await.unwrap();
    assert!(cpu_profile_header_is_valid(&description));
    let rows = drain(&mut source).await;
    assert_eq!(rows.len(), 7);
    for row in &rows {
        let TelemetryRecord::Operational(point) = row else { panic!("CPU profile operational rows") };
        assert!(cpu_profile_observation_fits(point, &description));
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count() + ReadSession::SESSION_SETUP_QUERIES, 10);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn tighter_caller_cannot_spend_setup_or_omit_the_final_native_identity() {
    let mut source = source(9, replies());
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 8);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let caller = CollectionBudget {
        max_queries: 6,
        max_snapshot_rows: 3,
        page_rows: 2,
        page_bytes: 512,
        total_bytes: 1024,
        statement_timeout_ms: 250,
        total_timeout_ms: 1000,
        ..Default::default()
    };
    let actual = process_cpu_budget(&caller);
    assert_eq!(actual, caller);
}
#[tokio::test]
async fn denied_cpu_family_preserves_original_totals_and_other_population_without_healthy_zero() {
    for index in [4, 5] {
        let mut rows = replies();
        rows[index] = Err(ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "fixture denial",
        )));
        let mut source = source(10, rows);
        let description = source.describe().await.unwrap();
        let retained = drain(&mut source).await;
        let header = description.observation.snapshot().unwrap();
        assert_eq!(header.sources[index - 3].state, SourceState::Unavailable);
        assert_eq!(header.sources[0].state, SourceState::Collected);
        assert!(!retained.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn socket_process_or_backward_final_clock_refuses_and_unknown_socket_stops_before_native_data() {
    for (field, value) in [("process_id", Val::Int(43)), ("observed_at", Val::Text("2020-01-01T06:00:00Z".into()))] {
        let mut rows = replies();
        rows[7] = Ok(vec![replace(&metadata(), &[(field, value)])]);
        let mut source = source(10, rows);
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut changed = source(10, replies());
    changed.describe().await.unwrap();
    drain(&mut changed).await;
    changed.io.generation = Some(2);
    assert_eq!(changed.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(changed.close().await, SourceShutdown::Quiescent);
    let mut unknown = source(10, replies());
    unknown.io.generation = None;
    assert!(unknown.describe().await.is_err());
    assert_eq!(unknown.query_count(), 2);
    assert_eq!(unknown.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn cancelled_native_family_stops_followup_and_preserves_original_shutdown_debt() {
    let mut rows = replies();
    rows[4] = Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
    let mut source = source(10, rows);
    source.io.inner.shutdown = SourceShutdown::Unconfirmed;
    assert_eq!(
        CollectionStop::from_error(&source.describe().await.unwrap_err()),
        CollectionStop::CancellationUnconfirmed
    );
    assert_eq!(source.query_count(), 5);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 5);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}
#[test]
fn only_boxed_mssql_instance_can_select_the_cpu_profile_without_default_or_pool_widening() {
    for subject in [
        CollectionSubject::Database,
        CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Pool,
                namespace: NativeIdentity::new("server"),
                native: NativeIdentity::new("pool"),
            },
        },
    ] {
        let mut request = request(10);
        request.subject = subject;
        assert!(request.validate().is_err());
    }
    let budget = process_cpu_budget(&CollectionBudget::upper_limits());
    assert_eq!(budget.max_queries, 10);
    assert_eq!(budget.max_snapshot_rows, 69);
    assert_eq!(budget.page_bytes, 64 * 1024);
    assert_eq!(budget.total_bytes, 256 * 1024);
    assert_eq!(budget.statement_timeout_ms, 1000);
    assert_eq!(budget.total_timeout_ms, 5000);
}
