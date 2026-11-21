use super::*;
use crate::telemetry::state::tests::{clock, pressure};
use crate::{
    read::{Parameter, ReadResult},
    resources::{
        pool::test_support::settings_member,
        test_support::{FakeRead, replace},
    },
    value::{Rec, Val},
};
use culprit_core::{Engine, SourceState};
const BEFORE: &str = "2020-01-01T06:00:00Z";
const FROM: &str = "2020-01-01T06:00:01Z";
const UNTIL: &str = "2020-01-01T06:00:02Z";
const FINAL: &str = "2020-01-01T06:00:03Z";
struct MemberRead {
    inner: FakeRead,
    generation: u64,
}
#[async_trait::async_trait]
impl NativeRead for MemberRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.inner.query(sql, parameters, limit).await
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(self.generation)
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}
fn member(at: &str) -> Rec {
    let row = settings_member(at);
    let extra = [
        ("selected_database_id", Val::Int(7)),
        ("database_id", Val::Int(7)),
        ("pool_engine_edition", Val::Int(5)),
        ("native_tier", Val::Text("General Purpose".into())),
        ("server_performance_state", Val::Int(1)),
        ("metadata_visibility", Val::Int(1)),
    ];
    Rec::of(
        &row.columns()
            .chain(extra.iter().map(|(name, value)| (*name, value)))
            .map(|(name, value)| (name, value.clone()))
            .collect::<Vec<_>>(),
    )
}
fn wait() -> Rec {
    Rec::of(&[
        ("wait_type", Val::Text("WRITELOG".into())),
        ("waiting_tasks_count", Val::Int(2)),
        ("wait_time_ms", Val::Int(5)),
        ("signal_wait_time_ms", Val::Int(1)),
        ("max_wait_time_ms", Val::Int(3)),
    ])
}
fn request(total: u32) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Pool,
                namespace: NativeIdentity::new("fixture-server").qualified("kind", "azure-sql-logical-server"),
                native: NativeIdentity::new("fixture-pool"),
            },
        },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::WaitCounters },
        budget: CollectionBudget { max_queries: total, ..Default::default() },
    }
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![member(BEFORE)]),
        Ok(vec![clock(true, FROM)]),
        Ok(vec![pressure(false)]),
        Ok(vec![wait()]),
        Ok(vec![member(UNTIL)]),
        Ok(vec![member(FINAL)]),
    ]
}
fn source(rows: Vec<ReadResult<Vec<Rec>>>, total: u32) -> StateSource<MemberRead> {
    let request = request(total);
    let budget = ReadSession::budget_after_setup(bounded_budget(&request.budget)).unwrap();
    StateSource::with_io(
        MemberRead { inner: FakeRead::new(rows).with_query_limit(budget.max_queries), generation: 1 },
        &request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap()
}
async fn drain(source: &mut StateSource<MemberRead>) -> Vec<TelemetryRecord> {
    let mut rows = vec![];
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::WaitCounterSamples, cursor.as_deref()).await.unwrap()
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
async fn original_pool_waits_reuse_native_membership_and_state_owner_in_seven_reads_plus_one_set() {
    let mut source = source(replies(), 8);
    let original = source.describe().await.unwrap();
    let authority = mssql_pool_current_authority(&original).unwrap().unwrap();
    assert_eq!(authority.before.database_id, 7);
    assert_eq!(authority.before.observed_at, "2020-01-01T06:00:00.000000000Z");
    let context = NativeReadContext::from_snapshot(&original).unwrap();
    assert_eq!(context.observed_from, "2020-01-01T06:00:01.000000000Z");
    assert_eq!(context.observed_until, "2020-01-01T06:00:02.000000000Z");
    assert_eq!(context.database_writable, Some(true));
    let rows = drain(&mut source).await;
    let TelemetryRecord::WaitCounter(row) = &rows[0] else { panic!() };
    assert_eq!(row.scope, culprit_core::MeasurementScope::Pool);
    assert_eq!(row.reset, culprit_core::ResetMarker::Unknown);
    assert!(row.fits_source(&original));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 7);
    for index in [1, 5, 6] {
        let membership = &source.io.inner.calls[index];
        assert!(membership.sql.contains("SELECT TOP (1) CAST(1 AS int) FROM sys.fn_my_permissions(NULL,N'SERVER')"));
        assert!(membership.sql.contains("granted_permission.permission_name=N'VIEW SERVER PERFORMANCE STATE'"));
        assert!(!membership.sql.contains("HAS_PERMS_BY_NAME(NULL,NULL"));
        assert!(!membership.sql.contains("VIEW DATABASE STATE") && !membership.sql.contains("IS_SRVROLEMEMBER"));
        assert_eq!(membership.limit, 2);
        assert!(membership.parameters.is_empty());
    }
    let call = &source.io.inner.calls[4];
    assert!(
        call.sql.contains("TOP (@P1)")
            && call.sql.contains("sys.dm_os_wait_stats")
            && call.sql.contains("ORDER BY wait_type")
    );
    assert_eq!(call.limit, 501);
    assert!(matches!(call.parameters[0], Parameter::Integer(501)));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn total_seven_cannot_spend_the_set_reservation_or_omit_final_membership() {
    let mut source = source(replies(), 7);
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.err().unwrap()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let caller = CollectionBudget {
        max_queries: 99,
        max_snapshot_rows: 3,
        page_rows: 2,
        page_bytes: 1024,
        total_bytes: 2048,
        statement_timeout_ms: 250,
        total_timeout_ms: 1000,
        ..Default::default()
    };
    let actual = bounded_budget(&caller);
    assert_eq!(actual.max_queries, 8);
    assert_eq!(actual.max_snapshot_rows, 3);
    assert_eq!(actual.page_rows, 2);
    assert_eq!(actual.page_bytes, 1024);
    assert_eq!(actual.total_bytes, 2048);
    assert_eq!(actual.statement_timeout_ms, 250);
    assert_eq!(actual.total_timeout_ms, 1000);
}
#[tokio::test]
async fn hyperscale_unknown_tier_or_permission_never_reads_or_publishes_pool_waits_or_healthy_empty() {
    for (name, value) in [
        ("native_tier", Val::Text("Hyperscale".into())),
        ("native_tier", Val::Null),
        ("server_performance_state", Val::Null),
        ("server_performance_state", Val::Int(0)),
    ] {
        let mut rows = replies();
        rows.remove(4).expect("the positive fixture contains the wait response");
        rows.remove(3).expect("the positive fixture contains the admission response");
        for index in [1, 3, 4] {
            let data = rows[index].as_mut().unwrap();
            data[0] = replace(&data[0], &[(name, value.clone())]);
        }
        let mut source = source(rows, 8);
        let original = source.describe().await.unwrap();
        let coverage = &original.observation.snapshot().unwrap().sources[0];
        assert_eq!(coverage.state, SourceState::Unavailable);
        assert!(!coverage.complete_scope);
        assert_eq!(coverage.records, 0);
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 5);
        assert!(!source.io.inner.calls.iter().any(|call| call.sql.contains("sys.dm_os_wait_stats")));
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn changed_native_membership_role_visibility_and_backward_final_clock_refuse_the_original() {
    for (name, value) in [
        ("pool_name", Val::Text("foreign".into())),
        ("incarnation", Val::Text("33333333-3333-3333-3333-333333333333".into())),
        ("physical_incarnation", Val::Text("44444444-4444-4444-4444-444444444444".into())),
        ("selected_database_id", Val::Int(8)),
        ("native_tier", Val::Text("Hyperscale".into())),
        ("server_performance_state", Val::Null),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("observed_at", Val::Text(FROM.into())),
    ] {
        let mut rows = replies();
        rows[6] = Ok(vec![replace(&member(FINAL), &[(name, value)])]);
        let mut source = source(rows, 8);
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{name}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    for at in [UNTIL, FINAL] {
        let mut rows = replies();
        rows[6] = Ok(vec![member(at)]);
        let mut source = source(rows, 8);
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut source = source(replies(), 8);
    source.describe().await.unwrap();
    drain(&mut source).await;
    source.io.generation = 2;
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn changed_closing_membership_prevents_publication_and_permission_failure_is_distinct_from_empty() {
    let mut rows = replies();
    rows[5] = Ok(vec![replace(&member(UNTIL), &[("pool_name", Val::Text("foreign".into()))])]);
    let mut stopped = source(rows, 8);
    assert!(stopped.describe().await.is_err());
    assert_eq!(stopped.close().await, SourceShutdown::Quiescent);
    for denied in [false, true] {
        let mut rows = replies();
        rows[4] = if denied {
            Err(ReadError::Source(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "owned denial")))
        } else {
            Ok(vec![])
        };
        let mut source = source(rows, 8);
        let original = source.describe().await.unwrap();
        assert_eq!(
            original.observation.snapshot().unwrap().sources[0].state,
            if denied { SourceState::Unavailable } else { SourceState::Empty }
        );
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn cancellation_debt_never_triggers_membership_followup_or_verified_finish() {
    let mut rows = replies();
    rows[4] = Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
    let mut source = source(rows, 8);
    source.io.inner.shutdown = SourceShutdown::Unconfirmed;
    assert_eq!(
        CollectionStop::from_error(&source.describe().await.err().unwrap()),
        CollectionStop::CancellationUnconfirmed
    );
    assert_eq!(source.query_count(), 5);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 5);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}

#[tokio::test]
async fn foreign_requested_pool_unknown_native_member_and_secondary_stop_before_data_or_admission() {
    for (name, value) in [
        ("pool_name", Val::Text("foreign-pool".into())),
        ("server_name", Val::Text("foreign-server".into())),
        ("pool_name", Val::Null),
        ("incarnation", Val::Text("00000000-0000-0000-0000-000000000000".into())),
        ("database_id", Val::Int(0)),
        ("updateability", Val::Text("READ_ONLY".into())),
    ] {
        let mut rows = replies();
        rows[1] = Ok(vec![replace(&member(BEFORE), &[(name, value)])]);
        let mut source = source(rows, 8);
        assert!(source.describe().await.is_err(), "{name}");
        assert_eq!(source.query_count(), 2);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut rows = replies();
    rows[0] = Ok(vec![Rec::of(&[("edition", Val::Int(3))])]);
    let mut source = source(rows, 8);
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn failed_effective_server_permission_witness_never_falls_back_to_database_visibility() {
    for (error, shutdown) in [
        (
            ReadError::Source(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denial")),
            SourceShutdown::Quiescent,
        ),
        (
            ReadError::Source(culprit_core::Error::new(culprit_core::ErrorCode::QueryFailed, "fixture query failure")),
            SourceShutdown::Quiescent,
        ),
        (ReadError::Stopped(CollectionStop::TimedOut), SourceShutdown::Quiescent),
        (ReadError::Stopped(CollectionStop::CancellationUnconfirmed), SourceShutdown::Unconfirmed),
    ] {
        let expected = match &error {
            ReadError::Source(error) => error.code,
            ReadError::Stopped(stop) => stop.error().code,
        };
        let mut rows = replies();
        rows[1] = Err(error);
        let mut source = source(rows, 8);
        source.io.inner.shutdown = shutdown;
        assert_eq!(source.describe().await.err().unwrap().code, expected);
        assert_eq!(source.query_count(), 2);
        assert!(source.finish().await.is_err());
        assert_eq!(source.query_count(), 2, "failed authority cannot start a final-membership query");
        assert!(!source.io.inner.calls.iter().any(|call| call.sql.contains("sys.dm_os_wait_stats")));
        assert_eq!(source.close().await, shutdown);
    }
}
